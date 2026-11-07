//! envd's data plane driven end to end by its real consumers: the client
//! library over envd's own connection loop, the tools' data conformance
//! suite, the tools' transfer orchestration against a blob store, and the
//! builtin file tools over a remote environment.

use std::path::{Path, PathBuf};

use environment_client::{ConnectOptions, DataClient};
use environment_daemon::{DaemonRuntime, config::DaemonConfig, server};
use environment_protocol::{
    data::{
        exec::{ExecState, ReturnOn, StoppedBy},
        process::{ProcessSignal, ReadProcessParams, TerminateProcessParams},
    },
    error::ProtocolErrorKind,
    handshake::ImplementationInfo,
    ids::ProcessId,
    paths::{EnvironmentPath, PathConvention},
};
use tools::{
    environment::process::{
        ContinueProcessRequest, ProcessExecutor, ProcessRequest, ProcessStatus,
    },
    environment_protocol::{RemoteEnvironmentConnection, conformance},
};

fn wire_path(path: &Path) -> EnvironmentPath {
    let text = path.to_str().expect("UTF-8 test path");
    #[cfg(windows)]
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    EnvironmentPath::from_native(
        text,
        if cfg!(windows) {
            PathConvention::Windows
        } else {
            PathConvention::Posix
        },
    )
    .expect("environment path")
}

fn command(posix: &str, windows: &str) -> Vec<String> {
    if cfg!(windows) {
        [
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            windows,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else {
        ["sh", "-c", posix].into_iter().map(str::to_owned).collect()
    }
}

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

async fn connect(runtime: &DaemonRuntime) -> DataClient {
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(server::serve_stream(runtime.clone(), server_io));
    let options =
        ConnectOptions::new(ImplementationInfo::new("envd-round-trip-test").expect("name"));
    let (client, _) = DataClient::connect_initialized_over(client_io, options)
        .await
        .expect("initialized");
    client
}

/// A daemon over a fresh root, its state dir beside the root, and an
/// initialized client connected to it through the real connection loop.
struct Daemon {
    _temp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    runtime: DaemonRuntime,
    client: DataClient,
}

impl Daemon {
    async fn start() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        #[cfg(windows)]
        let base = PathBuf::from(wire_path(&base).to_native());
        let root = base.join("root");
        std::fs::create_dir_all(&root).expect("root");
        let runtime =
            DaemonRuntime::new(daemon_config(&root, base.join("state"))).expect("runtime");
        let client = connect(&runtime).await;
        Self {
            _temp: temp,
            base,
            root,
            runtime,
            client,
        }
    }

    fn path(&self, relative: &str) -> EnvironmentPath {
        let path = if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        };
        wire_path(&path)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn envd_passes_the_data_conformance_suite() {
    let daemon = Daemon::start().await;
    let outside = daemon.base.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    let mut suite = conformance::DataConformance::new(daemon.client.clone(), daemon.path(""));
    suite.outside_root = Some(wire_path(&outside));
    let report = conformance::run(&suite).await;
    if let Err(failure) = report.into_result() {
        panic!("{failure}");
    }
}

/// The actual model adapters edit, build and test in a native Windows cwd.
/// This requires the Rust toolchain and MSVC build environment, like the suite.
#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn model_tools_edit_build_test_and_run_a_windows_job() {
    windows_model_task(&[]).await;
}

#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires PowerShell 7 on PATH and a standard Git for Windows installation"]
async fn model_tools_with_powershell_seven_and_git_bash() {
    use environment_protocol::data::platform::ShellKind;
    windows_model_task(&[ShellKind::PowerShell, ShellKind::GitBash]).await;
}

#[cfg(windows)]
async fn windows_model_task(required: &[environment_protocol::data::platform::ShellKind]) {
    use environment_protocol::data::{
        job::{JobReadEntry, JobReadRequest, JobStatus, ReadJobsParams},
        platform::ShellKind,
    };
    use harness::storage::InMemoryBlobStore;
    use serde_json::json;
    use std::sync::Arc;
    use tools::builtin::{
        BuiltinTool, BuiltinToolContext, BuiltinToolOperation as Op, BuiltinToolSurface as Surface,
    };
    let daemon = Daemon::start().await;
    let project = daemon.root.join("project space 雪");
    std::fs::create_dir(&project).unwrap();
    let initialized = daemon.client.initialize_response();
    initialized
        .validate_platform()
        .expect("consistent discovery");
    for kind in required {
        assert!(
            initialized
                .platform
                .shells
                .iter()
                .any(|shell| shell.kind == *kind),
            "required shell {kind:?} is unavailable: {:?}",
            initialized.platform
        );
    }
    if required.contains(&ShellKind::PowerShell) {
        assert_eq!(
            initialized.platform.default_shell,
            Some(ShellKind::PowerShell)
        );
    }
    assert!(matches!(
        initialized.platform.default_shell,
        Some(ShellKind::PowerShell | ShellKind::WindowsPowerShell)
    ));
    assert!(initialized.platform.shells.iter().any(|shell| shell.kind
        == ShellKind::WindowsPowerShell
        && shell.version.starts_with("5.1.")));
    assert!(
        !initialized
            .platform
            .shells
            .iter()
            .any(|shell| shell.kind == ShellKind::Bash)
    );
    let cwd = wire_path(&project);
    let facts = tools::environment::platform::describe("native-windows", initialized, &cwd);
    assert!(facts.contains("Operating system: Windows"));
    assert!(facts.contains(&cwd.to_native()));
    assert!(!facts.contains("transfers are unavailable"));
    let (_, ctx) = RemoteEnvironmentConnection::new(daemon.client.clone())
        .with_cwd(tools::fs::FsPath::from_environment(cwd.clone()))
        .into_contexts(Arc::new(InMemoryBlobStore::new()));
    let ctx = ctx
        .with_environment_id("native-windows")
        .with_operation_id("native-build-test");
    let invoke = async |op, surface, args| {
        BuiltinTool::environment(op, surface)
            .with_one_shot(true)
            .invoke_json(BuiltinToolContext::Environment(&ctx), args)
            .await
            .unwrap()
    };
    for shell in &initialized.platform.shells {
        let cmd = match shell.kind {
            ShellKind::PowerShell | ShellKind::WindowsPowerShell => {
                "[Console]::Write($PSVersionTable.PSVersion.ToString())"
            }
            ShellKind::GitBash => "printf '%s' \"$BASH_VERSION\"",
            ShellKind::Bash => panic!("Unix Bash advertised by Windows"),
        };
        let output = invoke(
            Op::RunProcess,
            Surface::CodexLike,
            json!({"cmd": cmd, "shell": shell.kind}),
        )
        .await;
        assert!(
            output.model_visible_text.contains(&shell.version),
            "{output:?}"
        );
    }
    invoke(Op::ApplyPatch, Surface::CodexLike, json!({"patch": "*** Begin Patch\n*** Add File: src\\main.rs\n+#[test]\n+fn unicode_output() { println!(\"hello 雪\"); assert_eq!(2 + 2, 4); }\n*** End Patch"})).await;
    let file = format!("{}\\src\\main.rs", cwd.to_native());
    let read_only = tools::fs::FsToolContext::new(
        Arc::new(tools::fs::ReadOnlyFileSystem::from_arc(
            ctx.filesystem.as_ref().unwrap().fs.clone(),
        )),
        ctx.blobs.clone(),
    )
    .with_cwd(tools::fs::FsPath::from_environment(cwd.clone()));
    let read = tools::fs::tools::invoke_read_file(
        &read_only,
        serde_json::from_value(json!({"path": file})).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(read.resolved_path.as_str(), format!("{cwd}/src/main.rs"));
    let read = invoke(
        Op::ReadFile,
        Surface::ClaudeCodeLike,
        json!({"file_path": file}),
    )
    .await;
    assert!(read.model_visible_text.contains("hello 雪"));
    invoke(
        Op::EditFile,
        Surface::ClaudeCodeLike,
        json!({"file_path": file, "old_string": "hello 雪", "new_string": "verified 雪"}),
    )
    .await;
    let glob = invoke(
        Op::Glob,
        Surface::Canonical,
        json!({"pattern": "**/*.rs", "path": "."}),
    )
    .await;
    assert!(glob.model_visible_text.contains("main.rs"));
    let grep = invoke(
        Op::Grep,
        Surface::Canonical,
        json!({"pattern": "verified", "path": "src"}),
    )
    .await;
    assert!(grep.model_visible_text.contains("verified"));
    let list = invoke(Op::ListDir, Surface::Canonical, json!({})).await;
    assert!(list.model_visible_text.contains("src"));
    invoke(
        Op::WriteFile,
        Surface::Canonical,
        json!({"path": "notes\\unicode.txt", "content": "雪"}),
    )
    .await;
    let built = invoke(Op::RunProcess, Surface::CodexLike, json!({"cmd": "rustc --test src/main.rs -o tests.exe; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; & .\\tests.exe --nocapture", "workdir": cwd.to_native(), "timeout_ms": 30000})).await;
    assert!(
        built.model_visible_text.contains("verified 雪"),
        "{}",
        built.model_visible_text
    );
    assert!(
        built.model_visible_text.contains("1 passed"),
        "{}",
        built.model_visible_text
    );
    let failed = invoke(
        Op::RunProcess,
        Surface::ClaudeCodeLike,
        json!({"command": "& $env:ComSpec /d /c 'exit 23'"}),
    )
    .await;
    assert_eq!(failed.output_json["exit_code"], 23, "{failed:?}");
    let unavailable = BuiltinTool::environment(Op::RunProcess, Surface::CodexLike)
        .invoke_json(
            BuiltinToolContext::Environment(&ctx),
            json!({"cmd": "echo wrong", "shell": "bash"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        unavailable,
        tools::error::ToolError::UnsupportedCapability { .. }
    ));
    // Direct argv and durable jobs bypass shell selection entirely.
    let direct = invoke(
        Op::RunProcess,
        Surface::Canonical,
        json!({"argv": [project.join("tests.exe").to_str().unwrap(), "--nocapture"], "cwd": "."}),
    )
    .await;
    assert_eq!(direct.output_json["exit_code"], 0, "{direct:?}");
    invoke(Op::JobSubmit, Surface::Canonical, json!({"jobs": [{"job_id": "native-test", "argv": [project.join("tests.exe").to_str().unwrap(), "--nocapture"], "cwd": cwd.to_native()}]})).await;
    let jobs = daemon
        .client
        .read_jobs(&ReadJobsParams {
            jobs: vec![JobReadRequest {
                job_id: environment_protocol::ids::JobId::new("native-test").unwrap(),
                after: None,
                tail_bytes: None,
                max_bytes: None,
            }],
            max_bytes: 4096,
            wait_ms: 10000,
            return_on: ReturnOn::Exit,
        })
        .await
        .unwrap();
    let JobReadEntry::Job { summary, output } = &jobs.jobs[0] else {
        panic!("{jobs:?}")
    };
    assert_eq!(summary.status, JobStatus::Succeeded, "{summary:?}");
    let bytes: Vec<_> = output
        .chunks
        .iter()
        .flat_map(|chunk| chunk.data.as_slice().iter().copied())
        .collect();
    assert!(String::from_utf8(bytes).unwrap().contains("verified 雪"));
    daemon.runtime.shutdown().cancel();
}

mod transfers {
    use super::*;
    use environment_client::ClientError;
    use environment_daemon::dispatch::{RequestContext, handle_request};
    use environment_protocol::{
        data::transfer::{TransferOnExisting, TransferPhase},
        jsonrpc::Response,
    };
    use harness::storage::BlobStore;
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tools::{
        builtin::{
            BuiltinTool, BuiltinToolContext, BuiltinToolOperation as Op,
            BuiltinToolSurface as Surface,
        },
        transfer,
    };

    /// A VFS tree survives materialize/capture and receipt retries. Unix tree
    /// replacement reuses destination bytes; capture into an empty store moves
    /// the missing content on both platforms.
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
                executable: cfg!(unix),
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
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&run).expect("run").permissions().mode() & 0o111,
            0o111
        );
        assert!(daemon.root.join("selected/empty").is_dir());

        let retry = transfer::materialize(
            &daemon.client,
            &store,
            "first",
            &selection,
            daemon.path("selected"),
            TransferOnExisting::Replace,
        )
        .await
        .expect("retry the receipt");
        assert_eq!(retry, first);

        #[cfg(unix)]
        {
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
        }

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
    fn in_process_client(
        runtime: DaemonRuntime,
    ) -> (DataClient, Arc<AtomicUsize>, Arc<AtomicBool>) {
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
                    if request.method == "transfer/commit"
                        && lose_commit.swap(false, Ordering::Relaxed)
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
            let base = std::path::PathBuf::from(wire_path(&base).to_native());
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
                .with_cwd(tools::fs::FsPath::from_environment(wire_path(&root)))
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
            #[cfg(unix)]
            std::fs::set_permissions(&result, std::fs::Permissions::from_mode(0o751))
                .expect("mode");
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
            #[cfg(unix)]
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
}

#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn windows_transfers_advertise_support_and_refuse_existing_tree_replacement() {
    let daemon = Daemon::start().await;
    let filesystem = daemon
        .client
        .initialize_response()
        .capabilities
        .filesystem
        .as_ref()
        .expect("filesystem");
    assert!(filesystem.write);
    assert!(filesystem.transfer);
    std::fs::create_dir(daemon.root.join("existing")).unwrap();
    std::fs::write(daemon.root.join("existing/keep"), b"original").unwrap();
    let error = daemon
        .client
        .transfer_begin(&environment_protocol::data::transfer::BeginTransferParams {
            operation_id: environment_protocol::ids::TransferOperationId::new("unsupported")
                .unwrap(),
            selection: environment_protocol::data::transfer::TransferSelection::Materialize {
                destination: daemon.path("existing"),
                on_existing: environment_protocol::data::transfer::TransferOnExisting::Replace,
            },
            limits: environment_protocol::data::transfer::TransferLimits {
                max_entries: 1,
                max_depth: 1,
                max_file_bytes: 1,
                max_total_bytes: 1,
                max_manifest_bytes: 1024,
                max_duration_ms: 1000,
            },
        })
        .await
        .expect_err("existing tree replacement unavailable");
    assert_eq!(
        error.protocol_kind(),
        Some(ProtocolErrorKind::CapabilityUnavailable)
    );
    assert_eq!(
        std::fs::read(daemon.root.join("existing/keep")).unwrap(),
        b"original"
    );
}

/// Skill discovery over envd's per-root scans: installer layouts under two
/// roots, per-root fingerprint caching, edits that do not change a skill's
/// metadata, and a malformed skill reported as a warning naming its root.
#[tokio::test(flavor = "current_thread")]
async fn skill_discovery_reads_installer_layouts_per_root() {
    use tools::skills::environment::{apply_cached_fingerprints, environment_skill_catalog};
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
            wire_path(&project_root).as_str().to_owned(),
            wire_path(&home_root).as_str().to_owned(),
        ]),
    };
    let mut query = tools::skills::environment::environment_skill_scan_query(
        &config,
        Some(wire_path(&daemon.root.join("project")).as_str()),
        None,
    )
    .expect("query");

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
        broken.warnings[0].starts_with(wire_path(&project_root).as_str()),
        "{:?}",
        broken.warnings
    );
}

/// Read a process by id until it exits. A start that has not landed yet answers
/// `notFound`, so that is retried too.
async fn read_until_exited(
    client: &DataClient,
    process_id: &ProcessId,
) -> environment_protocol::data::process::ReadProcessResponse {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "{process_id} did not exit"
        );
        let read = client
            .read_process(&ReadProcessParams {
                process_id: process_id.clone(),
                after: None,
                tail_bytes: None,
                ack: None,
                max_bytes: 4096,
                wait_ms: 250,
                return_on: ReturnOn::Exit,
            })
            .await;
        match read {
            Ok(response) if response.state == ExecState::Exited => return response,
            Ok(_) => {}
            Err(error) if error.protocol_kind() == Some(ProtocolErrorKind::NotFound) => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => panic!("read failed: {error}"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_a_run_process_call_terminates_only_a_command_with_no_handle() {
    let daemon = Daemon::start().await;
    let executor = RemoteEnvironmentConnection::new(daemon.client.clone())
        .process_executor()
        .expect("process capability");
    let run = |yield_ms| ProcessRequest {
        yield_ms,
        ..ProcessRequest::argv(command("sleep 60", "Start-Sleep -Seconds 60"))
    };

    // A handle reached the caller, so this one is the caller's to stop. It also
    // shows this executor's id sequence, which is the only way to name the next
    // command: the handle of a dropped call never comes back.
    let yielded = executor.run_process(run(Some(0))).await.expect("yielded");
    let handle = yielded.handle.expect("still running");
    let (prefix, sequence) = handle.as_str().rsplit_once('-').expect("sequenced id");
    let sequence: u64 = sequence.parse().expect("sequence");
    let next_id = |offset: u64| {
        ProcessId::new(format!("{prefix}-{}", sequence + offset)).expect("process id")
    };

    // Dropped during the first wait, once the start has landed.
    let mut waiting = Box::pin(executor.run_process(run(None)));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), &mut waiting)
            .await
            .is_err()
    );
    drop(waiting);
    let stopped = read_until_exited(&daemon.client, &next_id(1)).await;
    assert_eq!(stopped.stopped_by, Some(StoppedBy::Terminate));

    // Dropped after one poll, with its start possibly still in flight.
    let mut starting = Box::pin(executor.run_process(run(None)));
    assert!(
        tokio::time::timeout(std::time::Duration::ZERO, &mut starting)
            .await
            .is_err()
    );
    drop(starting);
    let stopped = read_until_exited(&daemon.client, &next_id(2)).await;
    assert_eq!(stopped.stopped_by, Some(StoppedBy::Terminate));

    let survivor = daemon
        .client
        .terminate_process(&TerminateProcessParams {
            process_id: ProcessId::new(handle.as_str()).expect("process id"),
            signal: ProcessSignal::Kill,
        })
        .await
        .expect("terminate");
    assert!(survivor.running, "a command with a handle keeps running");
}

#[tokio::test(flavor = "current_thread")]
async fn a_connection_lost_during_the_first_wait_leaves_a_handle_another_connection_can_use() {
    let daemon = Daemon::start().await;
    let doomed = connect(&daemon.runtime).await;
    let executor = RemoteEnvironmentConnection::new(doomed.clone())
        .process_executor()
        .expect("process capability");
    let go = daemon.root.join("go");
    let script = format!(
        "echo before; while [ ! -e '{}' ]; do sleep 0.05; done; echo after",
        go.display()
    );
    let windows = format!(
        "[Console]::Write('before' + [char]10); while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 50 }}; [Console]::Write('after' + [char]10)",
        go.display().to_string().replace('\'', "''")
    );

    let (output, _) = tokio::join!(
        executor.run_process(ProcessRequest::argv(command(&script, &windows))),
        async {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            doomed.close().await
        }
    );
    let output = output.expect("a lost connection is not a failed command");
    assert_eq!(output.status, ProcessStatus::Running);
    assert!(output.pid.is_some());
    assert!(output.failure.is_some());
    let handle = output.handle.expect("the handle comes back");

    std::fs::write(&go, b"").expect("release the command");
    let finished = RemoteEnvironmentConnection::new(daemon.client.clone())
        .process_executor()
        .expect("process capability")
        .continue_process(ContinueProcessRequest::wait(handle, None))
        .await
        .expect("the handle works on another connection");
    assert_eq!(finished.status, ProcessStatus::Succeeded);
    assert_eq!(finished.stdout.text_lossy(), "before\nafter\n");
}
