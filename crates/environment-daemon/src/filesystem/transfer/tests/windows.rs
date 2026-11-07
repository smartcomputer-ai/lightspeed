use super::*;
use crate::filesystem::transfer::{
    journal::{self, JournalFormat, StageRecord},
    publish::STAGE_PREFIX,
};

fn materialize(root: &Path, destination: &str) -> TransferSelection {
    TransferSelection::Materialize {
        destination: path(root, destination),
        on_existing: TransferOnExisting::Replace,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_inventory_validation_is_atomic_across_pages() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, true);
    begin(&fs, "names", materialize(&root, "output"))
        .await
        .unwrap();
    let page = |index, entries, last| AppendTransferInventoryParams {
        operation_id: id("names"),
        index,
        entries,
        last,
    };
    for name in [
        "NUL",
        "con.txt",
        "file:stream",
        "trailing.",
        "trailing ",
        "nested\\file",
    ] {
        let error = fs
            .transfer_append(
                &context(),
                page(
                    0,
                    vec![directory_entry("."), file_entry(name, b"", false)],
                    true,
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams, "{name}");
    }
    for (a, b) in [("Name", "name"), ("Ä", "ä")] {
        let error = fs
            .transfer_append(
                &context(),
                page(
                    0,
                    vec![
                        directory_entry("."),
                        file_entry(a, b"", false),
                        file_entry(b, b"", false),
                    ],
                    true,
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
    }
    let error = fs
        .transfer_append(
            &context(),
            page(
                0,
                vec![directory_entry("."), file_entry("run", b"", true)],
                true,
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
    let accepted = fs
        .transfer_append(
            &context(),
            page(0, vec![directory_entry("."), directory_entry("Dir")], false),
        )
        .await
        .unwrap();
    assert_eq!(accepted.entry_count, 2);
    let error = fs
        .transfer_append(&context(), page(2, vec![directory_entry("dir")], true))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
    let error = fs
        .transfer_append(
            &context(),
            page(2, vec![file_entry("dir/child", b"", false)], true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind(),
        ProtocolErrorKind::InvalidParams,
        "parents use the inventory's exact spelling"
    );
    fs.transfer_append(
        &context(),
        page(2, vec![file_entry("Dir/child", b"", false)], true),
    )
    .await
    .unwrap();
    advance_until(&fs, "names", TransferPhase::Ready)
        .await
        .unwrap();
    assert!(!root.join("output").exists());
    fs.transfer_commit(&context(), operation("names"))
        .await
        .unwrap();
    assert_eq!(std::fs::read(root.join("output/Dir/child")).unwrap(), b"");
}

#[tokio::test(flavor = "current_thread")]
async fn unicode_tree_publication_checks_chunks_and_keeps_ancestors_pinned() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, true);
    begin(&fs, "tree", materialize(&root, "parent ü/tree 空"))
        .await
        .unwrap();
    let bytes = b"chunked bytes";
    append_all(
        &fs,
        "tree",
        vec![
            directory_entry("."),
            directory_entry("nested ü"),
            file_entry("nested ü/空.txt", bytes, false),
            file_entry("empty", b"", false),
        ],
    )
    .await;
    advance_until(&fs, "tree", TransferPhase::Content)
        .await
        .unwrap();
    let chunk = |offset, data: &[u8]| WriteTransferContentParams {
        operation_id: id("tree"),
        digest: digest(bytes),
        offset,
        data: data.into(),
    };
    assert_eq!(
        fs.transfer_write(&context(), chunk(0, &bytes[..4]))
            .await
            .unwrap()
            .next_offset,
        4
    );
    assert_eq!(
        fs.transfer_write(&context(), chunk(0, &bytes[..4]))
            .await
            .unwrap()
            .next_offset,
        4
    );
    let error = fs
        .transfer_write(&context(), chunk(5, &bytes[5..]))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::TransferOffset));
    fs.transfer_write(&context(), chunk(4, &bytes[4..]))
        .await
        .unwrap();
    advance_until(&fs, "tree", TransferPhase::Ready)
        .await
        .unwrap();
    assert!(!root.join("parent ü/tree 空").exists());
    assert!(std::fs::rename(root.join("parent ü"), root.join("moved")).is_err());
    let receipt = fs
        .transfer_commit(&context(), operation("tree"))
        .await
        .unwrap();
    assert_eq!(receipt.phase, TransferPhase::Complete);
    assert_eq!(
        fs.transfer_commit(&context(), operation("tree"))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        std::fs::read(root.join("parent ü/tree 空/nested ü/空.txt")).unwrap(),
        bytes
    );
    let error = begin(&fs, "replace", materialize(&root, "parent ü/tree 空"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
    assert_eq!(
        std::fs::read(root.join("parent ü/tree 空/nested ü/空.txt")).unwrap(),
        bytes
    );
}

#[tokio::test(flavor = "current_thread")]
async fn publishers_cannot_overlap_through_case_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, true);
    begin(&fs, "first", materialize(&root, "Target"))
        .await
        .unwrap();
    let error = begin(&fs, "second", materialize(&root, "target"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.data,
        ErrorData::Busy {
            reason: Some(BusyReason::TransferCapacity),
            ..
        }
    ));
    fs.transfer_abort(&context(), operation("first"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_retains_busy_cleanup_and_quarantines_foreign_path_encodings() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let journal_dir = root.join(".state/transfers");
    {
        let fs = filesystem(&root, true);
        begin(&fs, "recover", materialize(&root, "out"))
            .await
            .unwrap();
        fs.transfer_abort(&context(), operation("recover"))
            .await
            .unwrap();
    }
    let stage_name = format!("{STAGE_PREFIX}0123456789abcdef0123456789abcdef");
    let parent = root.join("parent ü");
    std::fs::create_dir(&parent).unwrap();
    let staged = parent.join(&stage_name);
    std::fs::create_dir(&staged).unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(staged.join("locked"))
        .unwrap();
    let mut record = journal::read(&journal_dir, &id("recover"))
        .unwrap()
        .unwrap();
    record.stage = Some(StageRecord::new(&["parent ü".into()], &stage_name));
    record.status.receipt_expires_at_ms = Some(0);
    journal::write(&journal_dir, &id("recover"), &record).unwrap();
    drop(filesystem(&root, true));
    assert!(staged.exists());
    assert!(
        journal::read(&journal_dir, &id("recover"))
            .unwrap()
            .unwrap()
            .stage
            .is_some()
    );
    drop(held);
    drop(filesystem(&root, true));
    assert!(!staged.exists());
    assert!(
        journal::read(&journal_dir, &id("recover"))
            .unwrap()
            .is_none()
    );

    // Neither a foreign tagged record nor a legacy Unix record may cause
    // Windows to reinterpret its native path bytes for cleanup.
    std::fs::create_dir(&staged).unwrap();
    record.format = JournalFormat::PosixBytes;
    journal::write(&journal_dir, &id("foreign"), &record).unwrap();
    let mut legacy = serde_json::to_value(&record).unwrap();
    legacy.as_object_mut().unwrap().remove("format");
    std::fs::write(
        journal_dir.join("legacy.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    drop(filesystem(&root, true));
    assert!(staged.exists());
    assert!(journal_dir.join("foreign.quarantined").exists());
    assert!(journal_dir.join("legacy.quarantined").exists());
}

#[test]
fn journal_parent_segments_preserve_utf16_and_refuse_traversal() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let parent = vec![OsString::from("parent ü"), OsString::from_wide(&[0xd800])];
    let record = StageRecord::new(&parent, "unused");
    assert_eq!(record.parent_segments().unwrap(), parent);
    for parent in [
        vec![1],
        vec![b'.', 0, b'.', 0],
        vec![b'a', 0, b'/', 0, b'b', 0],
    ] {
        assert!(
            StageRecord {
                parent: vec![parent],
                name: "unused".into()
            }
            .parent_segments()
            .is_none()
        );
    }
}
