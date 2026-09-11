use super::*;
use async_trait::async_trait;
use engine::{
    BlobRef,
    storage::{BlobSource, BlobStore, BlobStoreError},
};
use environment_client::{EnvironmentClientResult, EnvironmentDataClient, JsonRpcTransport};
use environment_protocol::{
    data::{inventory::*, transfer_session::*},
    shared::EnvironmentPath,
};
use std::path::Path;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tools::{
    environment_protocol::RemoteEnvironmentConnection,
    transfer::{self},
};

struct Transport {
    runtime: DaemonRuntime,
    pending: Option<Value>,
    largest: Arc<AtomicUsize>,
    lose_commit: Arc<AtomicBool>,
}
#[async_trait]
impl JsonRpcTransport for Transport {
    async fn send(&mut self, message: Value) -> EnvironmentClientResult<()> {
        let wire = serde_json::to_vec(&message)?;
        self.largest.fetch_max(wire.len(), Ordering::Relaxed);
        let message: Value = serde_json::from_slice(&wire)?;
        let result = handle_data(
            &self.runtime,
            message["method"].as_str().unwrap(),
            message["params"].clone(),
        )
        .await;
        if message["params"]["action"] == "commit"
            && self.lose_commit.swap(false, Ordering::Relaxed)
        {
            return Err(environment_client::EnvironmentClientError::TransportClosed);
        }
        self.pending = Some(match result {
            Ok(result) => success_response(message["id"].clone(), result),
            Err(error) => error_response(Some(message["id"].clone()), error),
        });
        Ok(())
    }
    async fn recv(&mut self) -> EnvironmentClientResult<Option<Value>> {
        let Some(response) = self.pending.take() else {
            return Ok(None);
        };
        let wire = serde_json::to_vec(&response)?;
        self.largest.fetch_max(wire.len(), Ordering::Relaxed);
        Ok(Some(serde_json::from_slice(&wire)?))
    }
}
fn runtime(root: &Path, read_only: bool) -> DaemonRuntime {
    DaemonRuntime::new(crate::config::DaemonConfig {
        listen: None,
        cwd: root.into(),
        fs_root: root.into(),
        state_dir: root.join("state"),
        read_only_fs: read_only,
        registration: None,
        scrubbed_env: vec![],
    })
    .unwrap()
}
fn path(value: &str) -> EnvironmentPath {
    EnvironmentPath::new(value).unwrap()
}
fn remote(
    runtime: DaemonRuntime,
) -> (
    RemoteEnvironmentConnection<Transport>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
) {
    let largest = Arc::new(AtomicUsize::new(0));
    let lose_commit = Arc::new(AtomicBool::new(false));
    let caps = runtime.capabilities();
    (
        RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(Transport {
                runtime,
                pending: None,
                largest: largest.clone(),
                lose_commit: lose_commit.clone(),
            }),
            caps,
        ),
        largest,
        lose_commit,
    )
}
struct Repeated {
    remaining: u64,
}
#[async_trait]
impl BlobSource for Repeated {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
        let size = self.remaining.min(max_bytes as u64) as usize;
        self.remaining -= size as u64;
        Ok(vec![0x83; size])
    }
}
fn repeated_ref(size: u64) -> BlobRef {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let chunk = [0x83; 65536];
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(chunk.len() as u64) as usize;
        hash.update(&chunk[..n]);
        remaining -= n as u64;
    }
    BlobRef::parse(format!("sha256:{:x}", hash.finalize())).unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn rpc_vfs_roundtrip_streams_large_files_reuses_bytes_and_preserves_retry_receipt() {
    let environment = tempfile::tempdir().unwrap();
    let cas = tempfile::tempdir().unwrap();
    let store = store_fs::FsBlobStore::open(cas.path()).await.unwrap();
    let size = 10 * 1024 * 1024 + 7;
    let blob = repeated_ref(size);
    store
        .put_stream(&blob, size, &mut Repeated { remaining: size })
        .await
        .unwrap();
    let mut directory = vfs::VfsDirectory::default();
    directory.entries.insert(
        "run".into(),
        vfs::VfsEntry::File(vfs::VfsFile {
            blob_ref: blob.clone(),
            size_bytes: size,
            executable: true,
            media_type: None,
        }),
    );
    directory.entries.insert(
        "empty".into(),
        vfs::VfsEntry::Directory(vfs::VfsDirectory::default()),
    );
    let selection = vfs::VfsEntry::Directory(directory.clone());
    let (remote, largest, lose) = remote(runtime(environment.path(), false));
    let first = transfer::materialize(
        &remote,
        &store,
        "first",
        &selection,
        path("selected"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();
    assert_eq!(first.bytes, size);
    assert_eq!(first.transferred_bytes, size);
    assert_eq!(first.reused_bytes, 0);
    assert!(environment.path().join("selected/empty").is_dir());
    assert_eq!(
        std::fs::metadata(environment.path().join("selected/run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    use std::os::unix::fs::MetadataExt;
    let inode = std::fs::metadata(environment.path().join("selected/run"))
        .unwrap()
        .ino();
    let unchanged = transfer::materialize(
        &remote,
        &store,
        "unchanged",
        &selection,
        path("selected"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();
    assert_eq!(unchanged.transferred_bytes, 0);
    assert_eq!(unchanged.reused_bytes, size);
    assert_eq!(
        std::fs::metadata(environment.path().join("selected/run"))
            .unwrap()
            .ino(),
        inode,
        "identical complete trees need no restaging"
    );
    // Rename, mode-only edit and deletion reuse raw bytes while replacing the whole boundary.
    let vfs::VfsEntry::File(mut file) = directory.entries.remove("run").unwrap() else {
        unreachable!()
    };
    file.executable = false;
    directory.entries.remove("empty");
    directory
        .entries
        .insert("renamed".into(), vfs::VfsEntry::File(file));
    let selection = vfs::VfsEntry::Directory(directory);
    std::fs::write(environment.path().join("sibling"), b"preserved").unwrap();
    lose.store(true, Ordering::Relaxed);
    assert!(
        transfer::materialize(
            &remote,
            &store,
            "replace",
            &selection,
            path("selected"),
            TransferOnExisting::Replace
        )
        .await
        .is_err()
    );
    assert!(!environment.path().join("selected/run").exists());
    assert!(!environment.path().join("selected/empty").exists());
    std::fs::write(
        environment.path().join("selected/local-edit"),
        b"keep on retry",
    )
    .unwrap();
    let retry = transfer::materialize(
        &remote,
        &store,
        "replace",
        &selection,
        path("selected"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();
    assert_eq!(retry.phase, TransferPhase::Complete);
    assert_eq!(retry.transferred_bytes, 0);
    assert_eq!(retry.reused_bytes, size);
    assert!(environment.path().join("selected/local-edit").exists());
    std::fs::remove_file(environment.path().join("selected/local-edit")).unwrap();
    let captured = transfer::capture(&remote, &store, None, "capture-existing", path("selected"))
        .await
        .unwrap();
    assert_eq!(captured.entry, selection);
    assert_eq!(captured.status.transferred_bytes, 0);
    let empty_cas = tempfile::tempdir().unwrap();
    let empty_store = store_fs::FsBlobStore::open(empty_cas.path()).await.unwrap();
    let captured = transfer::capture(
        &remote,
        &empty_store,
        None,
        "capture-missing",
        path("selected"),
    )
    .await
    .unwrap();
    assert_eq!(captured.entry, selection);
    assert_eq!(captured.status.transferred_bytes, size);
    assert!(empty_store.has_blob(&blob).await.unwrap());
    assert!(
        largest.load(Ordering::Relaxed) < 512 * 1024,
        "every JSON message remains bounded"
    );
    assert_eq!(
        std::fs::read(environment.path().join("sibling")).unwrap(),
        b"preserved"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn inventories_are_paged_and_twenty_gib_files_do_not_block_begin_or_one_advance() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("many")).unwrap();
    for index in 0..300 {
        std::fs::write(temp.path().join(format!("many/{index:04}")), b"same").unwrap();
    }
    let runtime = runtime(temp.path(), false);
    let fs = runtime.filesystem();
    fs.transfer(TransferRequest::Begin {
        operation_id: "pages".into(),
        selection: TransferSelection::Capture {
            source: path("many"),
        },
        limits: Default::default(),
    })
    .await
    .unwrap();
    loop {
        if let TransferResponse::Status(status) = fs
            .transfer(TransferRequest::Advance {
                operation_id: "pages".into(),
            })
            .await
            .unwrap()
            && status.phase == TransferPhase::Ready
        {
            break;
        }
    }
    let mut offset = 0;
    let mut count = 0;
    loop {
        let TransferResponse::Inventory {
            entries,
            next_offset,
        } = fs
            .transfer(TransferRequest::Inventory {
                operation_id: "pages".into(),
                offset,
            })
            .await
            .unwrap()
        else {
            panic!()
        };
        assert!(entries.len() <= MAX_INVENTORY_PAGE);
        count += entries.len();
        if let Some(next) = next_offset {
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(count, 301);
    let file = std::fs::File::create(temp.path().join("twenty-gib")).unwrap();
    file.set_len(20 * 1024 * 1024 * 1024).unwrap();
    let TransferResponse::Status(start) = fs
        .transfer(TransferRequest::Begin {
            operation_id: "large".into(),
            selection: TransferSelection::Capture {
                source: path("twenty-gib"),
            },
            limits: Default::default(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(start.phase, TransferPhase::Scanning);
    let TransferResponse::Status(step) = fs
        .transfer(TransferRequest::Advance {
            operation_id: "large".into(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(step.phase, TransferPhase::Scanning);
    fs.transfer(TransferRequest::Abort {
        operation_id: "large".into(),
    })
    .await
    .unwrap();
    assert_eq!(file.metadata().unwrap().len(), 20 * 1024 * 1024 * 1024);
}
#[tokio::test(flavor = "current_thread")]
async fn scan_fingerprints_content_and_filters_large_unrelated_files() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("skills")).unwrap();
    std::fs::write(temp.path().join("skills/SKILL.md"), b"first").unwrap();
    std::fs::File::create(temp.path().join("skills/large"))
        .unwrap()
        .set_len(20 * 1024 * 1024 * 1024)
        .unwrap();
    let runtime = runtime(temp.path(), true);
    let fs = runtime.filesystem();
    let mut query = ScanParams {
        follow_symlinks: false,
        roots: vec![path("skills")],
        include_patterns: vec!["**/SKILL.md".into(), "SKILL.md".into()],
        read_content: true,
        digest_algorithm: None,
        limits: Default::default(),
        if_none_match: None,
    };
    let first = fs.scan(query.clone()).await.unwrap();
    assert!(first.complete);
    assert_eq!(first.entries.len(), 1);
    query.if_none_match = first.fingerprint;
    assert!(fs.scan(query.clone()).await.unwrap().unchanged);
    std::fs::write(temp.path().join("skills/SKILL.md"), b"other").unwrap();
    assert!(!fs.scan(query.clone()).await.unwrap().unchanged);
    query.roots.push(path("missing"));
    let absent = fs.scan(query).await.unwrap();
    assert!(absent.complete);
    assert!(!absent.unchanged);
    assert!(absent.diagnostics.is_empty());
    let metadata = fs
        .scan(ScanParams {
            follow_symlinks: false,
            roots: vec![path("skills/large")],
            include_patterns: vec![],
            read_content: false,
            digest_algorithm: None,
            limits: Default::default(),
            if_none_match: None,
        })
        .await
        .unwrap();
    assert!(metadata.complete);
    assert!(matches!(
        metadata.entries[0].content,
        ScanContent::File {
            digest: None,
            size_bytes: 21_474_836_480,
            ..
        }
    ));
    let hashed = fs
        .scan(ScanParams {
            follow_symlinks: false,
            roots: vec![path("skills/SKILL.md")],
            include_patterns: vec![],
            read_content: false,
            digest_algorithm: Some(ScanDigestAlgorithm::Sha256),
            limits: Default::default(),
            if_none_match: None,
        })
        .await
        .unwrap();
    assert!(hashed.complete);
    assert!(
        matches!(&hashed.entries[0].content,ScanContent::File {digest:Some(digest),..} if digest==BlobRef::from_bytes(b"other").as_str())
    );
}
#[tokio::test(flavor = "current_thread")]
async fn rpc_materialize_creates_missing_parents_for_files_and_trees() {
    let environment = tempfile::tempdir().unwrap();
    std::fs::write(environment.path().join("sibling"), b"preserved").unwrap();
    let (remote, _, _) = remote(runtime(environment.path(), false));
    let store = engine::storage::InMemoryBlobStore::new();
    let entry = vfs::VfsEntry::File(vfs::VfsFile {
        blob_ref: store.put_bytes(b"content".to_vec()).await.unwrap(),
        size_bytes: 7,
        executable: true,
        media_type: None,
    });
    let mut directory = vfs::VfsDirectory::default();
    directory.entries.insert("file".into(), entry.clone());
    for (id, entry, on_existing) in [
        ("file", entry, TransferOnExisting::Error),
        (
            "tree",
            vfs::VfsEntry::Directory(directory),
            TransferOnExisting::Replace,
        ),
    ] {
        let destination = path(&format!("created/{id}/nested/selected"));
        let status = transfer::materialize(
            &remote,
            &store,
            id,
            &entry,
            destination.clone(),
            on_existing,
        )
        .await
        .unwrap();
        assert_eq!(status.phase, TransferPhase::Complete);
        let captured =
            transfer::capture(&remote, &store, None, &format!("capture-{id}"), destination)
                .await
                .unwrap();
        assert_eq!(captured.entry, entry);
    }
    assert_eq!(
        std::fs::read(environment.path().join("sibling")).unwrap(),
        b"preserved"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn secure_replacement_rejects_links_and_uses_private_staging_without_reading_write_only_targets()
 {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"untouched").unwrap();
    symlink(outside.path(), temp.path().join("escape")).unwrap();
    symlink("absent", temp.path().join("dangling")).unwrap();
    std::fs::write(temp.path().join("blocked"), b"not a directory").unwrap();
    let read_only = runtime(temp.path(), true);
    let runtime = runtime(temp.path(), false);
    let fs = runtime.filesystem();
    for (index, destination) in [
        "escape/secret",
        "escape/missing/secret",
        "dangling/missing/secret",
        "blocked/missing/secret",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            fs.transfer(TransferRequest::Begin {
                operation_id: format!("invalid-parent-{index}"),
                selection: TransferSelection::Materialize {
                    destination: path(destination),
                    on_existing: TransferOnExisting::Replace
                },
                limits: Default::default()
            })
            .await
            .unwrap_err()
            .code,
            EnvironmentProtocolErrorCode::Forbidden
        );
    }
    assert!(!outside.path().join("missing").exists());
    assert!(!temp.path().join("absent").exists());
    assert_eq!(
        std::fs::read(outside.path().join("secret")).unwrap(),
        b"untouched"
    );
    assert_eq!(
        std::fs::read(temp.path().join("blocked")).unwrap(),
        b"not a directory"
    );
    assert_eq!(
        read_only
            .filesystem()
            .transfer(TransferRequest::Begin {
                operation_id: "read-only-parents".into(),
                selection: TransferSelection::Materialize {
                    destination: path("read-only/missing/target"),
                    on_existing: TransferOnExisting::Replace
                },
                limits: Default::default()
            })
            .await
            .unwrap_err()
            .code,
        EnvironmentProtocolErrorCode::CapabilityUnavailable
    );
    assert!(!temp.path().join("read-only").exists());
    std::fs::write(temp.path().join("target"), b"old").unwrap();
    std::fs::set_permissions(
        temp.path().join("target"),
        std::fs::Permissions::from_mode(0o200),
    )
    .unwrap();
    fs.transfer(TransferRequest::Begin {
        operation_id: "private".into(),
        selection: TransferSelection::Materialize {
            destination: path("target"),
            on_existing: TransferOnExisting::Replace,
        },
        limits: Default::default(),
    })
    .await
    .unwrap();
    for entry in std::fs::read_dir(temp.path()).unwrap() {
        let entry = entry.unwrap();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".env-transfer-")
        {
            assert_eq!(
                entry.metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
    assert_eq!(
        fs.transfer(TransferRequest::Begin {
            operation_id: "overlap".into(),
            selection: TransferSelection::Materialize {
                destination: path("target/nested"),
                on_existing: TransferOnExisting::Replace
            },
            limits: Default::default()
        })
        .await
        .unwrap_err()
        .code,
        EnvironmentProtocolErrorCode::Conflict
    );
    fs.transfer(TransferRequest::Abort {
        operation_id: "private".into(),
    })
    .await
    .unwrap();
    let (remote, _, _) = remote(runtime.clone());
    let store = engine::storage::InMemoryBlobStore::new();
    let blob = store.put_bytes(b"new".to_vec()).await.unwrap();
    let entry = vfs::VfsEntry::File(vfs::VfsFile {
        blob_ref: blob,
        size_bytes: 3,
        executable: false,
        media_type: None,
    });
    transfer::materialize(
        &remote,
        &store,
        "write-only",
        &entry,
        path("target"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(temp.path().join("target")).unwrap(), b"new");
    assert_eq!(
        std::fs::read(outside.path().join("secret")).unwrap(),
        b"untouched"
    );
    transfer::materialize(
        &remote,
        &store,
        "to-directory",
        &vfs::VfsEntry::Directory(vfs::VfsDirectory::default()),
        path("target"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();
    assert!(temp.path().join("target").is_dir());
    transfer::materialize(
        &remote,
        &store,
        "back-to-file",
        &entry,
        path("target"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();

    let capture = fs
        .transfer(TransferRequest::Begin {
            operation_id: "observe".into(),
            selection: TransferSelection::Capture {
                source: path("target"),
            },
            limits: Default::default(),
        })
        .await
        .unwrap();
    assert!(matches!(capture, TransferResponse::Status(_)));
    loop {
        if let TransferResponse::Status(status) = fs
            .transfer(TransferRequest::Advance {
                operation_id: "observe".into(),
            })
            .await
            .unwrap()
            && status.phase == TransferPhase::Ready
        {
            break;
        }
    }
    std::fs::write(temp.path().join("target"), b"changed").unwrap();
    assert_eq!(
        fs.transfer(TransferRequest::Commit {
            operation_id: "observe".into()
        })
        .await
        .unwrap_err()
        .code,
        EnvironmentProtocolErrorCode::Conflict
    );
}

#[tokio::test(flavor = "current_thread")]
async fn persisted_receipts_survive_restart_and_interrupted_operations_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let cas = tempfile::tempdir().unwrap();
    let store = store_fs::FsBlobStore::open(cas.path()).await.unwrap();
    let blob = store.put_bytes(b"content".to_vec()).await.unwrap();
    let entry = vfs::VfsEntry::File(vfs::VfsFile {
        blob_ref: blob,
        size_bytes: 7,
        executable: false,
        media_type: None,
    });
    {
        let (remote, _, _) = remote(runtime(temp.path(), false));
        transfer::materialize(
            &remote,
            &store,
            "completed",
            &entry,
            path("target"),
            TransferOnExisting::Replace,
        )
        .await
        .unwrap();
        transfer::capture(&remote, &store, None, "captured", path("target"))
            .await
            .unwrap();
        use tools::transfer::EnvironmentTransfer;
        remote
            .request(TransferRequest::Begin {
                operation_id: "interrupted".into(),
                selection: TransferSelection::Materialize {
                    destination: path("target"),
                    on_existing: TransferOnExisting::Replace,
                },
                limits: Default::default(),
            })
            .await
            .unwrap();
    }
    std::fs::write(temp.path().join("target"), b"later edit").unwrap();
    let (remote, _, _) = remote(runtime(temp.path(), false));
    let receipt = transfer::materialize(
        &remote,
        &store,
        "completed",
        &entry,
        path("target"),
        TransferOnExisting::Replace,
    )
    .await
    .unwrap();
    assert_eq!(receipt.phase, TransferPhase::Complete);
    assert_eq!(
        std::fs::read(temp.path().join("target")).unwrap(),
        b"later edit"
    );
    let capture = transfer::capture(&remote, &store, None, "captured", path("target"))
        .await
        .unwrap();
    assert_eq!(capture.entry, entry);
    assert!(
        transfer::materialize(
            &remote,
            &store,
            "interrupted",
            &entry,
            path("target"),
            TransferOnExisting::Replace
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read(temp.path().join("target")).unwrap(),
        b"later edit"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn bad_chunks_invalid_manifests_and_read_only_requests_never_publish() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("old"), b"keep").unwrap();
    let runtime = runtime(temp.path(), false);
    let fs = runtime.filesystem();
    fs.transfer(TransferRequest::Begin {
        operation_id: "invalid".into(),
        selection: TransferSelection::Materialize {
            destination: path("old"),
            on_existing: TransferOnExisting::Replace,
        },
        limits: Default::default(),
    })
    .await
    .unwrap();
    loop {
        if let TransferResponse::Status(status) = fs
            .transfer(TransferRequest::Advance {
                operation_id: "invalid".into(),
            })
            .await
            .unwrap()
            && status.phase == TransferPhase::Inventory
        {
            break;
        }
    }
    let digest = BlobRef::from_bytes(b"new").to_string();
    let file = |path: &str| InventoryEntry {
        path: path.into(),
        content: InventoryContent::File {
            size_bytes: 3,
            executable: false,
            digest: digest.clone(),
        },
    };
    for bad in ["../escape", "/absolute", "a//b", "a/../b", "a\\b"] {
        assert_eq!(
            fs.transfer(TransferRequest::Append {
                operation_id: "invalid".into(),
                offset: 0,
                entries: vec![file(bad)],
                last: true
            })
            .await
            .unwrap_err()
            .code,
            EnvironmentProtocolErrorCode::InvalidRequest
        );
    }
    let page = TransferRequest::Append {
        operation_id: "invalid".into(),
        offset: 0,
        entries: vec![file("")],
        last: true,
    };
    fs.transfer(page.clone()).await.unwrap();
    fs.transfer(page).await.unwrap();
    assert_eq!(
        fs.transfer(TransferRequest::Write {
            operation_id: "invalid".into(),
            digest: digest.clone(),
            offset: 0,
            data: vec![0; MAX_CONTENT_CHUNK + 1].into()
        })
        .await
        .unwrap_err()
        .code,
        EnvironmentProtocolErrorCode::InvalidRequest
    );
    assert_eq!(
        fs.transfer(TransferRequest::Write {
            operation_id: "invalid".into(),
            digest: digest.clone(),
            offset: 0,
            data: b"bad".as_slice().into()
        })
        .await
        .unwrap_err()
        .code,
        EnvironmentProtocolErrorCode::Conflict
    );
    assert_eq!(std::fs::read(temp.path().join("old")).unwrap(), b"keep");
    let chunk = TransferRequest::Write {
        operation_id: "invalid".into(),
        digest,
        offset: 0,
        data: b"new".as_slice().into(),
    };
    fs.transfer(chunk.clone()).await.unwrap();
    fs.transfer(chunk).await.unwrap();
    fs.transfer(TransferRequest::Abort {
        operation_id: "invalid".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        fs.transfer(TransferRequest::Commit {
            operation_id: "invalid".into()
        })
        .await
        .unwrap_err()
        .code,
        EnvironmentProtocolErrorCode::Conflict
    );
    let read_only = crate::server::streaming_transfer_tests::runtime(temp.path(), true);
    assert_eq!(
        read_only
            .filesystem()
            .transfer(TransferRequest::Begin {
                operation_id: "denied".into(),
                selection: TransferSelection::Materialize {
                    destination: path("old"),
                    on_existing: TransferOnExisting::Replace
                },
                limits: Default::default()
            })
            .await
            .unwrap_err()
            .code,
        EnvironmentProtocolErrorCode::CapabilityUnavailable
    );
    assert_eq!(std::fs::read(temp.path().join("old")).unwrap(), b"keep");
}

#[tokio::test(flavor = "current_thread")]
async fn configured_cwd_aliases_work_under_the_host_root() {
    let temp = tempfile::tempdir().unwrap();
    let alias = temp.path().join("cwd-alias");
    let actual = temp.path().join("actual");
    std::fs::create_dir(&actual).unwrap();
    symlink(&actual, &alias).unwrap();
    std::fs::write(actual.join("file"), b"bytes").unwrap();
    let fs = crate::filesystem::LocalFileSystem::new(Path::new("/").into(), alias, true);
    let result = fs
        .scan(ScanParams {
            follow_symlinks: false,
            roots: vec![path("file")],
            include_patterns: vec![],
            read_content: true,
            digest_algorithm: None,
            limits: Default::default(),
            if_none_match: None,
        })
        .await
        .unwrap();
    assert!(result.complete, "{:?}", result.diagnostics);
    assert_eq!(
        result.entries[0].data.as_ref().unwrap().as_slice(),
        b"bytes"
    );
    symlink(&actual, actual.join("user-link")).unwrap();
    let result = fs
        .scan(ScanParams {
            follow_symlinks: false,
            roots: vec![path("user-link/file")],
            include_patterns: vec![],
            read_content: true,
            digest_algorithm: None,
            limits: Default::default(),
            if_none_match: None,
        })
        .await
        .unwrap();
    assert!(!result.complete, "user-selected symlinks remain rejected");
}

#[tokio::test(flavor = "current_thread")]
async fn scan_installer_layouts_aliases_collisions_and_semantic_publication() {
    use tools::skills::environment::*;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let cwd = root.join("project/subdir");
    let canonical = root.join("installed/review");
    for directory in [
        &home,
        &cwd,
        &canonical,
        &root.join("project/.agents/skills"),
        &home.join(".codex/skills/copy"),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let doc = "---\nname: review\ndescription: >-\n  Review code\n  carefully.\nmetadata:\n  hooks: [ignored]\n---\nbody one";
    std::fs::write(canonical.join("SKILL.md"), doc).unwrap();
    std::fs::write(home.join(".codex/skills/copy/SKILL.md"), doc).unwrap();
    symlink(&canonical, root.join("project/.agents/skills/review")).unwrap();
    symlink(&canonical, root.join("project/.agents/skills/alias")).unwrap();
    let config = engine::EnvironmentSkillsConfig {
        roots: Some(vec![
            root.join("project/.agents/skills")
                .to_string_lossy()
                .into_owned(),
            home.join(".codex/skills").to_string_lossy().into_owned(),
        ]),
    };
    let runtime = runtime(&root, true);
    let mut query = environment_skill_scan_query(&config, cwd.to_str(), home.to_str()).unwrap();
    let first = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(first.complete, "{:?}", first.diagnostics);
    let catalog = environment_skill_catalog("machine-a", &first).unwrap();
    assert_eq!(
        catalog.skills.len(),
        2,
        "aliases collapse but independent copies remain"
    );
    assert!(
        catalog
            .skills
            .iter()
            .all(|skill| skill.description == "Review code carefully.")
    );
    assert_ne!(catalog.skills[0].skill_id, catalog.skills[1].skill_id);
    let other = environment_skill_catalog("machine-b", &first).unwrap();
    assert_ne!(catalog.skills[0].skill_id, other.skills[0].skill_id);
    query.if_none_match = first.fingerprint;
    assert!(
        runtime
            .filesystem()
            .scan(query.clone())
            .await
            .unwrap()
            .unchanged
    );
    std::fs::write(canonical.join("SKILL.md"), doc.replace("one", "two")).unwrap();
    let body_edit = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(!body_edit.unchanged);
    assert_eq!(
        catalog,
        environment_skill_catalog("machine-a", &body_edit).unwrap()
    );
    assert!(
        String::from_utf8(std::fs::read(canonical.join("SKILL.md")).unwrap())
            .unwrap()
            .ends_with("body two")
    );
    let blobs = engine::storage::InMemoryBlobStore::new();
    let Some(engine::CoreAgentCommand::UpsertContext { entry, .. }) =
        publish_environment_skill_catalog(&blobs, None, &catalog)
            .await
            .unwrap()
    else {
        panic!("publication");
    };
    assert!(
        publish_environment_skill_catalog(
            &blobs,
            Some(&entry),
            &environment_skill_catalog("machine-a", &body_edit).unwrap()
        )
        .await
        .unwrap()
        .is_none()
    );
    let prefix = blobs.read_bytes(&entry.content.content_ref).await.unwrap();
    std::fs::write(
        canonical.join("SKILL.md"),
        doc.replace("carefully", "thoroughly"),
    )
    .unwrap();
    let edited = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(
        publish_environment_skill_catalog(
            &blobs,
            Some(&entry),
            &environment_skill_catalog("machine-a", &edited).unwrap()
        )
        .await
        .unwrap()
        .is_some()
    );
    assert_eq!(
        blobs.read_bytes(&entry.content.content_ref).await.unwrap(),
        prefix
    );
    std::fs::remove_file(canonical.join("SKILL.md")).unwrap();
    let removed = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert_eq!(
        environment_skill_catalog("machine-a", &removed)
            .unwrap()
            .skills
            .len(),
        1
    );
    std::fs::write(
        canonical.join("SKILL.md"),
        "---\nname: broken\ndescription: [invalid]\n---",
    )
    .unwrap();
    let invalid = environment_skill_catalog(
        "machine-a",
        &runtime.filesystem().scan(query).await.unwrap(),
    )
    .unwrap();
    assert_eq!(invalid.skills.len(), 1);
    assert_eq!(invalid.warnings.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn scan_retarget_scope_loops_and_aggregate_limits_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    for name in ["one", "two"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
        std::fs::write(temp.path().join(name).join("SKILL.md"), "same").unwrap();
    }
    symlink(temp.path().join("one"), temp.path().join("alias")).unwrap();
    let runtime = runtime(temp.path(), true);
    let mut query = ScanParams {
        roots: vec![path("alias")],
        include_patterns: vec!["**/SKILL.md".into(), "SKILL.md".into()],
        read_content: true,
        follow_symlinks: true,
        digest_algorithm: Some(ScanDigestAlgorithm::Sha256),
        limits: InventoryLimits::default(),
        if_none_match: None,
    };
    let first = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(first.complete);
    query.if_none_match = first.fingerprint;
    let parent_scope = environment_daemon_scope_for_test(temp.path());
    assert!(
        !parent_scope.scan(query.clone()).await.unwrap().unchanged,
        "changed access scope cannot authorize reuse"
    );
    let mut shallow = query.clone();
    shallow.limits.max_depth = 0;
    let truncated = runtime.filesystem().scan(shallow).await.unwrap();
    assert!(!truncated.complete && !truncated.unchanged);
    let mut directories = query.clone();
    directories.include_patterns.clear();
    directories.read_content = false;
    directories.digest_algorithm = None;
    assert!(
        runtime
            .filesystem()
            .scan(directories)
            .await
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry.content, ScanContent::Directory))
    );
    std::fs::remove_file(temp.path().join("alias")).unwrap();
    symlink(temp.path().join("two"), temp.path().join("alias")).unwrap();
    let retargeted = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(retargeted.complete && !retargeted.unchanged);
    assert_ne!(
        first.entries[0].canonical_path,
        retargeted.entries[0].canonical_path
    );
    assert!(
        matches!(&retargeted.entries[0].content, ScanContent::File { digest: Some(digest), .. } if digest == BlobRef::from_bytes(b"same").as_str())
    );
    query.if_none_match = retargeted.fingerprint;
    query.read_content = false;
    assert!(
        !runtime
            .filesystem()
            .scan(query.clone())
            .await
            .unwrap()
            .unchanged
    );
    query.roots = vec![path("one"), path("two")];
    query.limits.max_total_bytes = 7;
    let limited = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(!limited.complete && !limited.unchanged && limited.fingerprint.is_none());
    assert!(!limited.diagnostics.is_empty());
    query.limits = InventoryLimits::default();
    query.roots = vec![path("alias")];
    symlink(temp.path().join("two"), temp.path().join("two/loop")).unwrap();
    assert!(
        !runtime
            .filesystem()
            .scan(query.clone())
            .await
            .unwrap()
            .complete
    );
    std::fs::remove_file(temp.path().join("two/loop")).unwrap();
    std::fs::remove_file(temp.path().join("alias")).unwrap();
    symlink(outside.path(), temp.path().join("alias")).unwrap();
    let escaped = runtime.filesystem().scan(query.clone()).await.unwrap();
    assert!(!escaped.complete);
    assert_eq!(
        escaped.diagnostics[0].error.code,
        EnvironmentProtocolErrorCode::Forbidden
    );
    std::fs::remove_file(temp.path().join("alias")).unwrap();
    symlink(temp.path().join("missing"), temp.path().join("alias")).unwrap();
    let dangling = runtime.filesystem().scan(query).await.unwrap();
    assert!(!dangling.complete && !dangling.unchanged);
}

fn environment_daemon_scope_for_test(cwd: &std::path::Path) -> crate::filesystem::LocalFileSystem {
    crate::filesystem::LocalFileSystem::new(
        cwd.parent().unwrap().to_path_buf(),
        cwd.to_path_buf(),
        false,
    )
}
