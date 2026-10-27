//! envd's data plane driven end to end by its real consumers: the client
//! library over envd's own connection loop, the tools' data conformance
//! suite, the tools' transfer orchestration against a blob store, and the
//! builtin file tools over a remote environment.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use environment_client::{ClientError, ConnectOptions, DataClient};
use environment_daemon::{
    DaemonRuntime,
    config::DaemonConfig,
    dispatch::{RequestContext, handle_request},
    server,
};
use environment_protocol::{
    data::transfer::{TransferOnExisting, TransferPhase},
    handshake::ImplementationInfo,
    jsonrpc::Response,
    paths::EnvironmentPath,
};
use harness::storage::BlobStore;
use serde_json::json;
use tools::{
    builtin::{
        BuiltinTool, BuiltinToolContext, BuiltinToolOperation as Op, BuiltinToolSurface as Surface,
    },
    environment_protocol::{RemoteEnvironmentConnection, conformance},
    transfer,
};

fn daemon_config(root: &Path, state_dir: PathBuf) -> DaemonConfig {
    DaemonConfig {
        listen: None,
        cwd: root.to_path_buf(),
        fs_root: root.to_path_buf(),
        state_dir,
        read_only_fs: false,
        registration: None,
        scrubbed_env: Vec::new(),
    }
}

/// A daemon over a fresh root, its state dir beside the root, and an
/// initialized client connected to it through the real connection loop.
struct Daemon {
    _temp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    client: DataClient,
}

impl Daemon {
    async fn start() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let root = base.join("root");
        std::fs::create_dir_all(&root).expect("root");
        let runtime =
            DaemonRuntime::new(daemon_config(&root, base.join("state"))).expect("runtime");
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(server::serve_stream(runtime, server_io));
        let options =
            ConnectOptions::new(ImplementationInfo::new("envd-round-trip-test").expect("name"));
        let (client, _) = DataClient::connect_initialized_over(client_io, options)
            .await
            .expect("initialized");
        Self {
            _temp: temp,
            base,
            root,
            client,
        }
    }

    fn path(&self, relative: &str) -> EnvironmentPath {
        let path = if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        };
        EnvironmentPath::new(path.to_str().expect("UTF-8 temp path")).expect("environment path")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn envd_passes_the_data_conformance_suite() {
    let daemon = Daemon::start().await;
    let outside = daemon.base.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    let mut suite = conformance::DataConformance::new(daemon.client.clone(), daemon.path(""));
    suite.outside_root =
        Some(EnvironmentPath::new(outside.to_str().expect("UTF-8")).expect("outside path"));
    let report = conformance::run(&suite).await;
    if let Err(failure) = report.into_result() {
        panic!("{failure}");
    }
}

/// A VFS tree materialized into envd and captured back is the same tree;
/// a repeat reuses the bytes already there, and capturing into an empty
/// store moves them.
#[tokio::test(flavor = "current_thread")]
async fn vfs_trees_round_trip_through_materialize_and_capture() {
    let daemon = Daemon::start().await;
    let cas = tempfile::tempdir().expect("cas");
    let store = store_fs::FsBlobStore::open(cas.path())
        .await
        .expect("store");
    // Larger than one content chunk, so content moves in several writes.
    let size = 5 * 1024 * 1024 + 7;
    let content: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    let blob = store.put_bytes(content.clone()).await.expect("blob");
    let mut directory = vfs::VfsDirectory::default();
    directory.entries.insert(
        "run".into(),
        vfs::VfsEntry::File(vfs::VfsFile {
            blob_ref: blob.clone(),
            size_bytes: size as u64,
            media_type: None,
            executable: true,
        }),
    );
    directory.entries.insert(
        "empty".into(),
        vfs::VfsEntry::Directory(vfs::VfsDirectory::default()),
    );
    let selection = vfs::VfsEntry::Directory(directory);
    std::fs::write(daemon.root.join("sibling"), b"preserved").expect("sibling");

    let first = transfer::materialize(
        &daemon.client,
        &store,
        "first",
        &selection,
        daemon.path("selected"),
        TransferOnExisting::Replace,
    )
    .await
    .expect("materialize");
    assert_eq!(first.phase, TransferPhase::Complete);
    assert_eq!(first.total_bytes, size as u64);
    assert_eq!(first.transferred_bytes, size as u64);
    assert_eq!(first.reused_bytes, 0);
    let run = daemon.root.join("selected/run");
    assert_eq!(std::fs::read(&run).expect("run"), content);
    assert_eq!(
        std::fs::metadata(&run).expect("run").permissions().mode() & 0o111,
        0o111
    );
    assert!(daemon.root.join("selected/empty").is_dir());

    let again = transfer::materialize(
        &daemon.client,
        &store,
        "again",
        &selection,
        daemon.path("selected"),
        TransferOnExisting::Replace,
    )
    .await
    .expect("materialize again");
    assert_eq!(again.transferred_bytes, 0);
    assert_eq!(again.reused_bytes, size as u64);

    let captured = transfer::capture(
        &daemon.client,
        &store,
        None,
        "capture-known",
        daemon.path("selected"),
    )
    .await
    .expect("capture");
    assert_eq!(captured.entry, selection);
    assert_eq!(captured.status.transferred_bytes, 0);

    let empty_cas = tempfile::tempdir().expect("empty cas");
    let empty_store = store_fs::FsBlobStore::open(empty_cas.path())
        .await
        .expect("empty store");
    let captured = transfer::capture(
        &daemon.client,
        &empty_store,
        None,
        "capture-missing",
        daemon.path("selected"),
    )
    .await
    .expect("capture into an empty store");
    assert_eq!(captured.entry, selection);
    assert_eq!(captured.status.transferred_bytes, size as u64);
    assert!(empty_store.has_blob(&blob).await.expect("has blob"));
    assert_eq!(
        std::fs::read(daemon.root.join("sibling")).expect("sibling"),
        b"preserved"
    );
}

/// A client whose every request is dispatched straight into `runtime`,
/// recording the largest frame either way, and able to lose the response
/// of the next `transfer/commit` after the daemon acted on it.
fn in_process_client(runtime: DaemonRuntime) -> (DataClient, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let largest = Arc::new(AtomicUsize::new(0));
    let lose_commit = Arc::new(AtomicBool::new(false));
    let initialize = runtime.initialize_response();
    let client = DataClient::in_process(initialize, {
        let largest = largest.clone();
        let lose_commit = lose_commit.clone();
        move |request| {
            let runtime = runtime.clone();
            let largest = largest.clone();
            let lose_commit = lose_commit.clone();
            Box::pin(async move {
                largest.fetch_max(request.to_text().len(), Ordering::Relaxed);
                let result = handle_request(
                    &runtime,
                    &RequestContext::detached(),
                    &request.method,
                    request.params.clone(),
                )
                .await;
                let response = match result {
                    Ok(reply) => {
                        let response = Response::success(request.id.clone(), &reply.value);
                        if let Some(on_written) = reply.on_written {
                            on_written();
                        }
                        response
                    }
                    Err(error) => Response::failure(Some(request.id.clone()), error),
                };
                if request.method == "transfer/commit" && lose_commit.swap(false, Ordering::Relaxed)
                {
                    return Err(ClientError::Transport("response lost".to_owned()));
                }
                largest.fetch_max(response.to_text().len(), Ordering::Relaxed);
                Ok(response)
            })
        }
    });
    (client, largest, lose_commit)
}

/// The builtin `write_file` (from a blob reference) and `reference` tools
/// move large binary files through bounded transfer chunks on every tool
/// surface; a retried operation answers its first receipt, even after the
/// source changed, the file was edited, or the commit response was lost.
#[tokio::test(flavor = "current_thread")]
async fn blob_file_tools_stream_and_keep_receipts_across_retries() {
    for surface in [
        Surface::Canonical,
        Surface::CodexLike,
        Surface::ClaudeCodeLike,
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let root = base.join("root");
        std::fs::create_dir_all(&root).expect("root");
        let runtime =
            DaemonRuntime::new(daemon_config(&root, base.join("state"))).expect("runtime");
        let cas = tempfile::tempdir().expect("cas");
        let store = Arc::new(
            store_fs::FsBlobStore::open(cas.path())
                .await
                .expect("store"),
        );
        // Larger than the daemon's content chunk and its whole-file read ceiling.
        let size = 9 * 1024 * 1024 + 7;
        let content = store.put_bytes(vec![0x83; size]).await.expect("blob");
        let (client, largest, lose_commit) = in_process_client(runtime);
        let (_, context) = RemoteEnvironmentConnection::new(client)
            .with_cwd(tools::fs::FsPath::new(root.to_str().expect("UTF-8")).expect("cwd"))
            .into_contexts(store.clone());
        let context = context.with_environment_id("test-env");
        let key = if surface == Surface::ClaudeCodeLike {
            "file_path"
        } else {
            "path"
        };
        let write = BuiltinTool::environment(Op::WriteFile, surface);
        let capture = BuiltinTool::environment(Op::Reference, surface);

        let writer = context.clone().with_operation_id("write-binary");
        let output = write
            .invoke_json(
                BuiltinToolContext::Environment(&writer),
                json!({key: "nested/source.bin", "content_ref": content}),
            )
            .await
            .expect("write from reference");
        assert_eq!(output.output_json["bytes_written"], size);
        assert_eq!(output.output_json["receipt"]["total_bytes"], size);
        let source = root.join("nested/source.bin");
        assert_eq!(
            std::fs::metadata(&source).expect("written").len(),
            size as u64
        );

        let capture_cas = tempfile::tempdir().expect("capture cas");
        let capture_store = Arc::new(
            store_fs::FsBlobStore::open(capture_cas.path())
                .await
                .expect("capture store"),
        );
        let mut capture_context = context.clone().with_operation_id("capture-binary");
        capture_context.blobs = capture_store.clone();
        let captured = capture
            .invoke_json(
                BuiltinToolContext::Environment(&capture_context),
                json!({"path": "nested/source.bin"}),
            )
            .await
            .expect("capture");
        assert_eq!(captured.output_json["content_ref"], content.to_string());
        assert_eq!(captured.output_json["byte_len"], size);
        assert_eq!(captured.output_json["source"]["id"], "test-env");
        assert_eq!(captured.attachments.len(), 1);
        assert_eq!(
            capture_store
                .read_blob_range(&content, size as u64 - 7, 7)
                .await
                .expect("captured tail"),
            vec![0x83; 7]
        );

        std::fs::write(&source, b"changed live file").expect("change source");
        let changed = capture
            .invoke_json(
                BuiltinToolContext::Environment(&capture_context),
                json!({"path": "nested/source.bin"}),
            )
            .await
            .expect("retry after change");
        assert_eq!(
            changed.output_json, captured.output_json,
            "a retried capture answers the version it first captured"
        );
        std::fs::remove_file(&source).expect("remove source");
        let deleted = capture
            .invoke_json(
                BuiltinToolContext::Environment(&capture_context),
                json!({"path": "nested/source.bin"}),
            )
            .await
            .expect("retry after delete");
        assert_eq!(
            deleted.output_json, captured.output_json,
            "a completed capture never revisits the live path"
        );

        // A lost commit response leaves the write's receipt to answer the retry.
        let result = root.join("result.bin");
        std::fs::write(&result, b"original").expect("result");
        std::fs::set_permissions(&result, std::fs::Permissions::from_mode(0o751)).expect("mode");
        let writer = context.clone().with_operation_id("lost-write-receipt");
        lose_commit.store(true, Ordering::Relaxed);
        assert!(
            write
                .invoke_json(
                    BuiltinToolContext::Environment(&writer),
                    json!({key: "result.bin", "content_ref": content}),
                )
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::metadata(&result)
                .expect("result")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        std::fs::write(&result, b"later local edit").expect("edit");
        let retry = write
            .invoke_json(
                BuiltinToolContext::Environment(&writer),
                json!({key: "result.bin", "content_ref": content}),
            )
            .await
            .expect("retry the write");
        assert_eq!(retry.output_json["receipt"]["total_bytes"], size);
        assert_eq!(std::fs::read(&result).expect("result"), b"later local edit");

        std::fs::create_dir(root.join("directory")).expect("directory");
        std::fs::write(root.join("directory/keep"), b"keep").expect("keep");
        let writer = context.clone().with_operation_id("reject-directory-write");
        assert!(
            write
                .invoke_json(
                    BuiltinToolContext::Environment(&writer),
                    json!({key: "directory", "content_ref": content}),
                )
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.join("directory/keep")).expect("keep"),
            b"keep"
        );
        let reader = context
            .clone()
            .with_operation_id("reject-directory-capture");
        assert!(
            capture
                .invoke_json(
                    BuiltinToolContext::Environment(&reader),
                    json!({"path": "directory"}),
                )
                .await
                .is_err()
        );
        assert!(
            largest.load(Ordering::Relaxed) < 8 * 1024 * 1024,
            "large binary transfers move in bounded chunks"
        );
    }
}

/// Skill discovery over envd's per-root scans: installer layouts under two
/// roots, per-root fingerprint caching, edits that do not change a skill's
/// metadata, and a malformed skill reported as a warning naming its root.
#[tokio::test(flavor = "current_thread")]
async fn skill_discovery_reads_installer_layouts_per_root() {
    use tools::skills::environment::{
        apply_cached_fingerprints, environment_skill_catalog, environment_skill_scan_query,
    };
    let daemon = Daemon::start().await;
    let project_root = daemon.root.join("project/.agents/skills");
    let home_root = daemon.root.join("home/.codex/skills");
    let review = project_root.join("review");
    for directory in [&review, &home_root.join("copy")] {
        std::fs::create_dir_all(directory).expect("skill directory");
    }
    let doc = "---\nname: review\ndescription: >-\n  Review code\n  carefully.\n---\nbody one";
    std::fs::write(review.join("SKILL.md"), doc).expect("review");
    std::fs::write(home_root.join("copy/SKILL.md"), doc).expect("copy");
    let config = harness::EnvironmentSkillsConfig {
        roots: Some(vec![
            project_root.to_string_lossy().into_owned(),
            home_root.to_string_lossy().into_owned(),
        ]),
    };
    let cwd = daemon.root.join("project");
    let mut query = environment_skill_scan_query(&config, cwd.to_str(), None).expect("query");

    let first = daemon.client.scan(&query).await.expect("scan");
    let catalog = environment_skill_catalog("machine-a", &first, None).expect("catalog");
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    assert_eq!(catalog.skills.len(), 2, "independent copies stay separate");
    assert!(
        catalog
            .skills
            .iter()
            .all(|skill| skill.description == "Review code carefully.")
    );
    assert_ne!(catalog.skills[0].skill_id, catalog.skills[1].skill_id);
    let other = environment_skill_catalog("machine-b", &first, None).expect("other catalog");
    assert_ne!(catalog.skills[0].skill_id, other.skills[0].skill_id);
    assert_eq!(catalog.root_fingerprints.len(), 2);

    apply_cached_fingerprints(&mut query, &catalog);
    let unchanged = daemon.client.scan(&query).await.expect("rescan");
    let cached =
        environment_skill_catalog("machine-a", &unchanged, Some(&catalog)).expect("cached");
    assert_eq!(cached, catalog);

    std::fs::write(review.join("SKILL.md"), doc.replace("one", "two")).expect("body edit");
    let edited = daemon.client.scan(&query).await.expect("scan after edit");
    let edited =
        environment_skill_catalog("machine-a", &edited, Some(&catalog)).expect("edited catalog");
    assert_eq!(edited.skills, catalog.skills);

    std::fs::write(
        review.join("SKILL.md"),
        "---\nname: broken\ndescription: [invalid]\n---",
    )
    .expect("broken");
    apply_cached_fingerprints(&mut query, &edited);
    let broken = daemon.client.scan(&query).await.expect("scan broken");
    let broken =
        environment_skill_catalog("machine-a", &broken, Some(&edited)).expect("broken catalog");
    assert_eq!(broken.skills.len(), 1, "the other root's skill remains");
    assert_eq!(broken.warnings.len(), 1);
    assert!(
        broken.warnings[0].starts_with(project_root.to_str().expect("UTF-8")),
        "{:?}",
        broken.warnings
    );
}
