//! Data-plane conformance: drives any environment data server through
//! [`DataClient`] and checks the contract envd implements.
//!
//! The suite checks, against the wire and not through tool adapters:
//!
//! - the `initialize` response: plane, version, ceilings and capability
//!   families;
//! - `fs/*`: staged writes with `create` and `ifMatch` preconditions and a
//!   returned version, ranged reads with `fileSize` and `truncated`,
//!   ceilings refused as `invalidParams`, `fs/rename` without overwrite,
//!   paged `fs/readDirectory`, `fs/remove` preconditions, typed `notFound`
//!   and confinement errors, the shared pattern grammar of
//!   `fs/globFiles` and `fs/searchText` (and that the tools' generic glob
//!   fallback selects the same files), and per-root `fs/scan` results;
//! - confinement of symlinks (when the `process` family can create them):
//!   an in-root link is followed, a link that leaves the root is refused
//!   with `permissionDenied {outsideRoot}`;
//! - `process/*` (when the family is present): settled exits with codes and
//!   named signals, start-time input larger than a pipe buffer, tail reads,
//!   duplicate ids refused before anything runs, unknown handles as
//!   `notFound`, and graceful `terminate`;
//! - `job/*` (when the family is present): `requestId` replay versus
//!   `jobIdInUse`, per-entry `notFound`, and byte-exact paging.
//!
//! The suite works in a fresh directory below `work_directory` and removes
//! it at the end. Commands run through `/bin/sh`.
//!
//! ```no_run
//! # async fn example(client: environment_client::DataClient) {
//! use tools::environment_protocol::conformance::{DataConformance, run};
//!
//! let work = environment_protocol::paths::EnvironmentPath::new("/workspace").unwrap();
//! run(&DataConformance::new(client, work)).await.into_result().expect("data plane conforms");
//! # }
//! ```

use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    time::{Duration, Instant},
};

use environment_client::{ClientError, DataClient};
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    bytes::ByteChunk,
    data::{
        exec::{CommandSpec, ExecInput, ExecState, ExitStatus, ReturnOn, StoppedBy},
        fs::{
            CreateDirectoryParams, FileKind, GetMetadataParams, GlobFilesLimits, GlobFilesParams,
            ReadDirectoryParams, ReadFileParams, RemoveParams, RenameParams, SearchTextLimits,
            SearchTextParams, TreeFilter, WriteCondition, WriteFileParams, WriteFileResponse,
        },
        handshake::InitializeResponse,
        idle::GetIdleParams,
        job::{JobReadEntry, JobReadRequest, JobSpec, ReadJobsParams, StartJobsParams},
        process::{
            ProcessSignal, ReadProcessParams, ReadProcessResponse, StartProcessParams,
            TerminateProcessParams, WriteProcessParams,
        },
        scan::{ScanLimits, ScanParams, ScanRoot, ScanRootResult},
    },
    error::{ErrorData, PermissionReason, PreconditionReason, ProtocolErrorKind},
    handshake::Plane,
    ids::{JobId, JobRequestId, ProcessId},
    limits::MIN_CONCURRENT_REQUESTS,
    paths::{EnvironmentPath, TreePath},
};
use thiserror::Error;

/// What to run the suite against.
#[derive(Clone)]
pub struct DataConformance {
    /// An initialized connection to the server under test.
    pub client: DataClient,
    /// An existing, writable directory inside the filesystem root.
    pub work_directory: EnvironmentPath,
    /// A path outside the server's filesystem root, when the root is
    /// narrowed; checks that it is refused with `permissionDenied`.
    pub outside_root: Option<EnvironmentPath>,
    /// Bound on waiting for a command to exit. Default 30 s.
    pub command_timeout: Duration,
}

impl DataConformance {
    pub fn new(client: DataClient, work_directory: EnvironmentPath) -> Self {
        Self {
            client,
            work_directory,
            outside_root: None,
            command_timeout: Duration::from_secs(30),
        }
    }
}

impl fmt::Debug for DataConformance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataConformance")
            .field("work_directory", &self.work_directory)
            .field("outside_root", &self.outside_root)
            .finish_non_exhaustive()
    }
}

/// The outcome of one check.
#[derive(Clone, Debug)]
pub struct CheckResult {
    /// Stable check name, such as `fs.writeFile.preconditions`.
    pub name: &'static str,
    /// Why the check failed.
    pub failure: Option<String>,
}

/// Every check that ran, in order.
#[derive(Clone, Debug, Default)]
pub struct DataConformanceReport {
    pub checks: Vec<CheckResult>,
}

impl DataConformanceReport {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.failure.is_none())
    }

    pub fn failures(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|check| check.failure.is_some())
    }

    /// `Ok` when every check passed.
    pub fn into_result(self) -> Result<Self, DataConformanceFailure> {
        if self.passed() {
            Ok(self)
        } else {
            Err(DataConformanceFailure(self))
        }
    }
}

/// A report with at least one failed check.
#[derive(Clone, Debug, Error)]
pub struct DataConformanceFailure(pub DataConformanceReport);

impl fmt::Display for DataConformanceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "data conformance failed:")?;
        for check in self.0.failures() {
            write!(
                formatter,
                "\n- {}: {}",
                check.name,
                check.failure.as_deref().unwrap_or_default()
            )?;
        }
        Ok(())
    }
}

type Check = Result<(), String>;

/// Run every check that the server's capability families allow.
pub async fn run(suite: &DataConformance) -> DataConformanceReport {
    let mut report = DataConformanceReport::default();
    let initialized = suite.client.initialize_response().clone();
    let handshake = check_initialize(&initialized);
    let handshake_failed = handshake.is_err();
    record(&mut report, "initialize", handshake);
    if handshake_failed {
        return report;
    }
    record(
        &mut report,
        "env.getIdle",
        call(suite.client.get_idle(&GetIdleParams {}))
            .await
            .map(drop),
    );

    let work = match prepare(suite).await {
        Ok(work) => work,
        Err(failure) => {
            record(&mut report, "fs.prepare", Err(failure));
            return report;
        }
    };
    let context = Context {
        client: &suite.client,
        initialized: &initialized,
        work: &work,
        timeout: suite.command_timeout,
    };
    record(
        &mut report,
        "fs.writeFile.preconditions",
        context.write_preconditions().await,
    );
    record(
        &mut report,
        "fs.readFile.range",
        context.ranged_reads().await,
    );
    record(
        &mut report,
        "fs.readFile.notRegular",
        context.reads_refuse_directories().await,
    );
    record(&mut report, "fs.rename", context.rename().await);
    record(
        &mut report,
        "fs.readDirectory.paging",
        context.directory_paging().await,
    );
    record(&mut report, "fs.remove", context.remove().await);
    record(
        &mut report,
        "fs.errors",
        context.errors(suite.outside_root.as_ref()).await,
    );
    record(
        &mut report,
        "fs.globFiles.grammar",
        context.glob_grammar().await,
    );
    record(
        &mut report,
        "fs.glob.genericParity",
        context.glob_parity().await,
    );
    record(&mut report, "fs.searchText", context.search().await);
    record(&mut report, "fs.scan.perRoot", context.scan().await);
    if initialized.capabilities.process.is_some() {
        record(
            &mut report,
            "fs.confinement.symlinks",
            context
                .symlink_confinement(suite.outside_root.as_ref())
                .await,
        );
        record(&mut report, "process.exit", context.process_exit().await);
        record(
            &mut report,
            "process.signal",
            context.process_signal().await,
        );
        record(
            &mut report,
            "process.largeInput",
            context.process_large_input().await,
        );
        record(
            &mut report,
            "process.duplicateId",
            context.process_duplicate_id().await,
        );
        record(
            &mut report,
            "process.unknown",
            context.process_unknown().await,
        );
        record(
            &mut report,
            "process.terminate",
            context.process_terminate().await,
        );
    }
    if initialized.capabilities.job.is_some() {
        record(&mut report, "job.requestId", context.job_request_id().await);
        record(&mut report, "job.read.unknown", context.job_unknown().await);
        record(&mut report, "job.read.paging", context.job_paging().await);
    }
    record(
        &mut report,
        "fs.cleanup",
        call(suite.client.remove(&RemoveParams {
            path: work,
            recursive: true,
            force: true,
        }))
        .await
        .map(drop),
    );
    report
}

fn record(report: &mut DataConformanceReport, name: &'static str, outcome: Check) {
    report.checks.push(CheckResult {
        name,
        failure: outcome.err(),
    });
}

fn check_initialize(initialized: &InitializeResponse) -> Check {
    ensure(
        initialized.plane == Plane::Data,
        "initialize did not name the data plane",
    )?;
    ensure(
        initialized.protocol_version == ENVD_PROTOCOL_VERSION,
        format!(
            "protocolVersion {} differs from {ENVD_PROTOCOL_VERSION}",
            initialized.protocol_version
        ),
    )?;
    ensure(
        initialized.limits.max_concurrent_requests >= MIN_CONCURRENT_REQUESTS,
        "maxConcurrentRequests is below the protocol minimum",
    )?;
    let filesystem = initialized
        .capabilities
        .filesystem
        .as_ref()
        .ok_or("the suite needs the filesystem family")?;
    ensure(filesystem.write, "the suite needs a writable filesystem")
}

async fn prepare(suite: &DataConformance) -> Result<EnvironmentPath, String> {
    let work = join(
        &suite.work_directory,
        &format!("conformance-{}", uuid::Uuid::new_v4().simple()),
    )?;
    call(suite.client.create_directory(&CreateDirectoryParams {
        path: work.clone(),
        recursive: false,
    }))
    .await?;
    Ok(work)
}

struct Context<'a> {
    client: &'a DataClient,
    initialized: &'a InitializeResponse,
    work: &'a EnvironmentPath,
    timeout: Duration,
}

impl Context<'_> {
    fn path(&self, relative: &str) -> Result<EnvironmentPath, String> {
        let tree = TreePath::new(relative).map_err(|error| error.to_string())?;
        self.work.join(&tree).map_err(|error| error.to_string())
    }

    async fn write(&self, relative: &str, data: &[u8]) -> Result<WriteFileResponse, String> {
        call(self.client.write_file(&WriteFileParams {
            path: self.path(relative)?,
            data: ByteChunk::from(data),
            create: WriteCondition::Always,
            if_match: None,
            executable: false,
        }))
        .await
    }

    async fn mkdir(&self, relative: &str) -> Check {
        call(self.client.create_directory(&CreateDirectoryParams {
            path: self.path(relative)?,
            recursive: true,
        }))
        .await
        .map(drop)
    }

    async fn write_preconditions(&self) -> Check {
        let path = self.path("precondition.txt")?;
        let written = self.write("precondition.txt", b"one").await?;
        let read = call(self.client.read_file(&ReadFileParams {
            path: path.clone(),
            offset: 0,
            max_bytes: 16,
        }))
        .await?;
        ensure(
            read.version == written.version,
            "a read after a write reports a different version",
        )?;
        let absent = self
            .client
            .write_file(&WriteFileParams {
                path: path.clone(),
                data: ByteChunk::from(&b"two"[..]),
                create: WriteCondition::IfAbsent,
                if_match: None,
                executable: false,
            })
            .await;
        expect_precondition(absent, PreconditionReason::CreateCondition)?;
        let rewritten = call(self.client.write_file(&WriteFileParams {
            path: path.clone(),
            data: ByteChunk::from(&b"two"[..]),
            create: WriteCondition::IfPresent,
            if_match: Some(written.version.clone()),
            executable: false,
        }))
        .await?;
        ensure(
            rewritten.version != written.version,
            "a changed file kept its version",
        )?;
        let stale = self
            .client
            .write_file(&WriteFileParams {
                path,
                data: ByteChunk::from(&b"three"[..]),
                create: WriteCondition::Always,
                if_match: Some(written.version),
                executable: false,
            })
            .await;
        expect_precondition(stale, PreconditionReason::IfMatchFailed)
    }

    async fn ranged_reads(&self) -> Check {
        self.write("range.txt", b"0123456789").await?;
        let path = self.path("range.txt")?;
        let middle = call(self.client.read_file(&ReadFileParams {
            path: path.clone(),
            offset: 2,
            max_bytes: 3,
        }))
        .await?;
        ensure(
            middle.data.as_slice() == b"234",
            "offset and maxBytes select the range",
        )?;
        ensure(middle.file_size == 10, "fileSize is the whole file's size")?;
        ensure(
            middle.truncated,
            "truncated is set when bytes follow the range",
        )?;
        let end = call(self.client.read_file(&ReadFileParams {
            path: path.clone(),
            offset: 7,
            max_bytes: 10,
        }))
        .await?;
        ensure(
            end.data.as_slice() == b"789" && !end.truncated,
            "a range reaching the end is not truncated",
        )?;
        let ceiling = self.initialized.limits.filesystem.max_read_file_bytes;
        let above = self
            .client
            .read_file(&ReadFileParams {
                path,
                offset: 0,
                max_bytes: ceiling + 1,
            })
            .await;
        match above {
            Err(error) => match error.protocol_error().map(|error| &error.data) {
                Some(ErrorData::InvalidParams {
                    ceiling: Some(reported),
                    ..
                }) if *reported == ceiling => Ok(()),
                _ => Err(format!("maxBytes above the ceiling answered {error}")),
            },
            Ok(_) => Err("maxBytes above the ceiling was accepted, not refused".into()),
        }
    }

    async fn reads_refuse_directories(&self) -> Check {
        self.mkdir("directory").await?;
        let read = self
            .client
            .read_file(&ReadFileParams {
                path: self.path("directory")?,
                offset: 0,
                max_bytes: 16,
            })
            .await;
        match read {
            Err(error)
                if matches!(
                    error.protocol_error().map(|error| &error.data),
                    Some(ErrorData::FailedPrecondition {
                        reason: PreconditionReason::IsDirectory
                            | PreconditionReason::NotRegularFile
                    })
                ) =>
            {
                Ok(())
            }
            other => Err(format!("reading a directory answered {other:?}")),
        }
    }

    async fn rename(&self) -> Check {
        self.write("rename-a.txt", b"a").await?;
        self.write("rename-b.txt", b"b").await?;
        let rename = |source: &str, destination: &str, overwrite| {
            Ok::<_, String>(RenameParams {
                source: self.path(source)?,
                destination: self.path(destination)?,
                overwrite,
                if_match: None,
            })
        };
        expect_kind(
            self.client
                .rename(&rename("rename-a.txt", "rename-b.txt", false)?)
                .await,
            ProtocolErrorKind::AlreadyExists,
        )?;
        call(
            self.client
                .rename(&rename("rename-a.txt", "rename-c.txt", false)?),
        )
        .await?;
        expect_kind(
            self.client
                .rename(&rename("rename-a.txt", "rename-d.txt", false)?)
                .await,
            ProtocolErrorKind::NotFound,
        )?;
        let moved = call(self.client.read_file(&ReadFileParams {
            path: self.path("rename-c.txt")?,
            offset: 0,
            max_bytes: 16,
        }))
        .await?;
        ensure(
            moved.data.as_slice() == b"a",
            "rename did not move the content",
        )
    }

    async fn directory_paging(&self) -> Check {
        self.mkdir("listing").await?;
        for name in ["c", "a", "b"] {
            self.write(&format!("listing/{name}"), b"").await?;
        }
        let mut names = Vec::new();
        let mut cursor = None;
        for _ in 0..5 {
            let page = call(self.client.read_directory(&ReadDirectoryParams {
                path: self.path("listing")?,
                limit: 1,
                cursor,
            }))
            .await?;
            ensure(page.entries.len() <= 1, "a page exceeded its limit")?;
            names.extend(
                page.entries
                    .into_iter()
                    .map(|entry| entry.name.as_str().to_owned()),
            );
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        ensure(
            names == ["a", "b", "c"],
            format!("paged listing returned {names:?}, not the names in order"),
        )
    }

    async fn remove(&self) -> Check {
        self.mkdir("removal").await?;
        self.write("removal/file", b"x").await?;
        let remove = |recursive, force, relative: &str| {
            Ok::<_, String>(RemoveParams {
                path: self.path(relative)?,
                recursive,
                force,
            })
        };
        expect_precondition(
            self.client.remove(&remove(false, false, "removal")?).await,
            PreconditionReason::NotEmpty,
        )?;
        call(self.client.remove(&remove(true, false, "removal")?)).await?;
        call(self.client.remove(&remove(false, true, "removal")?)).await?;
        expect_kind(
            self.client.remove(&remove(false, false, "removal")?).await,
            ProtocolErrorKind::NotFound,
        )
    }

    async fn errors(&self, outside_root: Option<&EnvironmentPath>) -> Check {
        expect_kind(
            self.client
                .read_file(&ReadFileParams {
                    path: self.path("missing")?,
                    offset: 0,
                    max_bytes: 1,
                })
                .await,
            ProtocolErrorKind::NotFound,
        )?;
        if let Some(outside) = outside_root {
            let read = self
                .client
                .read_file(&ReadFileParams {
                    path: outside.clone(),
                    offset: 0,
                    max_bytes: 1,
                })
                .await;
            match read {
                Err(error)
                    if matches!(
                        error.protocol_error().map(|error| &error.data),
                        Some(ErrorData::PermissionDenied {
                            reason: Some(PermissionReason::OutsideRoot)
                        })
                    ) => {}
                other => return Err(format!("a path outside the root answered {other:?}")),
            }
        }
        Ok(())
    }

    async fn glob_grammar(&self) -> Check {
        self.mkdir("glob/sub").await?;
        self.write("glob/a.rs", b"").await?;
        self.write("glob/sub/b.rs", b"").await?;
        let ceilings = self.initialized.limits.filesystem.glob;
        let glob = |include: &str, max_depth| GlobFilesParams {
            root: self.path("glob").expect("glob root"),
            filter: TreeFilter {
                include: vec![include.to_owned()],
                max_depth,
                ..TreeFilter::default()
            },
            limits: GlobFilesLimits {
                max_matches: ceilings.max_matches.min(100),
                max_files: ceilings.max_files.min(1000),
                max_duration_ms: ceilings.max_duration_ms.min(10_000),
            },
        };
        let names = |matches: Vec<EnvironmentPath>| {
            matches
                .iter()
                .filter_map(|path| path.file_name().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let segment = call(self.client.glob_files(&glob("*.rs", None))).await?;
        ensure(
            names(segment.matches) == ["a.rs", "b.rs"],
            "a pattern without '/' matches names at any depth",
        )?;
        let anchored = call(self.client.glob_files(&glob("/*.rs", None))).await?;
        ensure(
            names(anchored.matches) == ["a.rs"],
            "'*' stays within one segment and a leading '/' anchors",
        )?;
        let shallow = call(self.client.glob_files(&glob("**/*.rs", Some(1)))).await?;
        ensure(
            names(shallow.matches) == ["a.rs"],
            "maxDepth 1 selects the root's direct children",
        )
    }

    /// The tools' `glob` over this server and over an in-memory filesystem
    /// holding the same tree select the same files: the generic fallback
    /// follows the grammar and the walk defaults of `fs/globFiles`.
    async fn glob_parity(&self) -> Check {
        use crate::fs::{
            CreateDirectoryOptions, FileSystem as _, FsPath, FsToolContext, InMemoryFileSystem,
            tools::{GlobArgs, invoke_glob},
        };

        const FILES: [&str; 10] = [
            "parity/a.rs",
            "parity/notes.md",
            "parity/sub/b.rs",
            "parity/sub/deep/c.rs",
            "parity/.hidden/d.rs",
            "parity/.e.rs",
            "parity/build/out.rs",
            "parity/skipped.rs",
            "parity/sub/.gitignore",
            "parity/.ignore",
        ];
        const PATTERNS: [&str; 8] = [
            "*.rs",
            "/*.rs",
            "sub/*.rs",
            "**/*.rs",
            "sub/**",
            "build/",
            "*.{rs,md}",
            "deep/*.rs",
        ];
        let memory = InMemoryFileSystem::full_access();
        let local_root = FsPath::new("/parity").map_err(|error| error.to_string())?;
        for file in FILES {
            let contents: &[u8] = match file {
                "parity/sub/.gitignore" => b"deep/\n",
                "parity/.ignore" => b"skipped.rs\n",
                _ => b"",
            };
            if let Some((directory, _)) = file.rsplit_once('/') {
                self.mkdir(directory).await?;
                memory
                    .create_directory(
                        &FsPath::new(format!("/{directory}")).map_err(|error| error.to_string())?,
                        CreateDirectoryOptions::recursive(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            self.write(file, contents).await?;
            memory
                .write_file(
                    &FsPath::new(format!("/{file}")).map_err(|error| error.to_string())?,
                    contents.to_vec(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        let blobs: std::sync::Arc<dyn harness::storage::BlobStore> =
            std::sync::Arc::new(harness::storage::InMemoryBlobStore::new());
        let remote_root =
            FsPath::new(self.path("parity")?.as_str()).map_err(|error| error.to_string())?;
        let remote = FsToolContext::new(
            std::sync::Arc::new(
                super::RemoteEnvironmentConnection::new(self.client.clone()).file_system(),
            ),
            blobs.clone(),
        );
        let local = FsToolContext::new(std::sync::Arc::new(memory), blobs);
        let relative = |root: &FsPath, matches: Vec<FsPath>| {
            matches
                .iter()
                .map(|path| {
                    path.as_str()
                        .strip_prefix(root.as_str())
                        .unwrap_or(path.as_str())
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        for pattern in PATTERNS {
            let args = |root: &FsPath| GlobArgs {
                pattern: pattern.to_owned(),
                path: Some(root.clone()),
                max_depth: None,
                limit: None,
            };
            let native = invoke_glob(&remote, args(&remote_root))
                .await
                .map_err(|error| format!("glob {pattern:?} on the server: {error}"))?;
            let generic = invoke_glob(&local, args(&local_root))
                .await
                .map_err(|error| format!("generic glob {pattern:?}: {error}"))?;
            let native = relative(&remote_root, native.matches);
            let generic = relative(&local_root, generic.matches);
            ensure(
                native == generic,
                format!(
                    "glob {pattern:?} selected {native:?} on the server but {generic:?} in the generic fallback"
                ),
            )?;
        }
        Ok(())
    }

    async fn search(&self) -> Check {
        self.mkdir("search").await?;
        self.write("search/notes.txt", b"alpha\nneedle here\n")
            .await?;
        let ceilings = self.initialized.limits.filesystem.search;
        let response = call(self.client.search_text(&SearchTextParams {
            root: self.path("search")?,
            pattern: "NEEDLE".into(),
            pattern_ignore_case: true,
            filter: TreeFilter {
                include: vec!["*.txt".into()],
                ..TreeFilter::default()
            },
            limits: SearchTextLimits {
                max_matches: ceilings.max_matches.min(10),
                max_files: ceilings.max_files.min(100),
                max_bytes: ceilings.max_bytes.min(1 << 20),
                max_duration_ms: ceilings.max_duration_ms.min(10_000),
                max_line_bytes: ceilings.max_line_bytes,
            },
        }))
        .await?;
        ensure(
            matches!(response.matches.as_slice(), [found] if found.line_number == 2 && found.line == "needle here"),
            format!("search answered {:?}", response.matches),
        )
    }

    async fn scan(&self) -> Check {
        self.mkdir("scan").await?;
        self.write("scan/one.md", b"# one").await?;
        let ceilings = self.initialized.limits.filesystem.scan;
        let response = call(self.client.scan(&ScanParams {
            roots: vec![
                ScanRoot {
                    path: self.path("scan")?,
                    if_none_match: None,
                    max_depth: None,
                },
                ScanRoot {
                    path: self.path("scan-missing")?,
                    if_none_match: None,
                    max_depth: None,
                },
                ScanRoot {
                    path: self.work.clone(),
                    if_none_match: None,
                    max_depth: Some(0),
                },
            ],
            filter: TreeFilter {
                include: vec!["*.md".into()],
                ..TreeFilter::default()
            },
            read_content: true,
            include_digests: false,
            follow_symlinks: false,
            limits: ScanLimits {
                max_entries: ceilings.max_entries.min(100),
                max_file_bytes: ceilings.max_file_bytes.min(4096),
                max_total_bytes: ceilings.max_total_bytes.min(65536),
                max_duration_ms: ceilings.max_duration_ms.min(10_000),
            },
        }))
        .await?;
        match response.roots.as_slice() {
            [
                ScanRootResult::Complete { entries, .. },
                ScanRootResult::Missing { .. },
                ScanRootResult::Complete { entries: probe, .. },
            ] if entries.len() == 1
                && entries[0].data.as_ref().map(ByteChunk::as_slice) == Some(&b"# one"[..])
                && probe.is_empty() =>
            {
                Ok(())
            }
            other => Err(format!("scan answered {other:?}")),
        }
    }

    /// An in-root symlink is followed; one whose target leaves the root is
    /// refused with `permissionDenied {outsideRoot}` by `fs/readFile` and a
    /// following `fs/getMetadata`, while the link itself stays describable.
    async fn symlink_confinement(&self, outside_root: Option<&EnvironmentPath>) -> Check {
        self.write("link-target.txt", b"inside").await?;
        let mut script = "ln -s link-target.txt link-inside".to_owned();
        if let Some(outside) = outside_root {
            script.push_str(&format!(" && ln -s '{outside}' link-outside"));
        }
        let process_id = self.start(&script, ExecInput::None {}).await?;
        let (_, exited) = self.read_to_exit(&process_id).await?;
        ensure(
            exited.exit == Some(ExitStatus::Code { code: 0 }),
            format!("creating the links exited {:?}", exited.exit),
        )?;
        let inside = call(self.client.read_file(&ReadFileParams {
            path: self.path("link-inside")?,
            offset: 0,
            max_bytes: 16,
        }))
        .await?;
        ensure(
            inside.data.as_slice() == b"inside",
            "reading an in-root link did not read its target",
        )?;
        let followed = call(self.client.get_metadata(&GetMetadataParams {
            path: self.path("link-inside")?,
            follow_symlinks: true,
        }))
        .await?;
        ensure(
            followed.metadata.kind == FileKind::File,
            format!(
                "a followed in-root link described a {:?}",
                followed.metadata.kind
            ),
        )?;
        if outside_root.is_none() {
            return Ok(());
        }
        let outside = self.path("link-outside")?;
        let link = call(self.client.get_metadata(&GetMetadataParams {
            path: outside.clone(),
            follow_symlinks: false,
        }))
        .await?;
        ensure(
            link.metadata.kind == FileKind::Symlink,
            "an escaping link is still describable as a symlink",
        )?;
        let outside_root = |result: Result<_, ClientError>, what: &str| match result {
            Err(error)
                if matches!(
                    error.protocol_error().map(|error| &error.data),
                    Some(ErrorData::PermissionDenied {
                        reason: Some(PermissionReason::OutsideRoot)
                    })
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(format!("{what} through an escaping link answered {error}")),
            Ok(_) => Err(format!("{what} followed a link that leaves the root")),
        };
        outside_root(
            self.client
                .read_file(&ReadFileParams {
                    path: outside.clone(),
                    offset: 0,
                    max_bytes: 16,
                })
                .await
                .map(drop),
            "fs/readFile",
        )?;
        outside_root(
            self.client
                .get_metadata(&GetMetadataParams {
                    path: outside,
                    follow_symlinks: true,
                })
                .await
                .map(drop),
            "fs/getMetadata {followSymlinks}",
        )
    }

    fn process_id(&self) -> ProcessId {
        ProcessId::new(format!("conformance-{}", uuid::Uuid::new_v4().simple()))
            .expect("generated ids fit the grammar")
    }

    async fn start(&self, script: &str, input: ExecInput) -> Result<ProcessId, String> {
        let process_id = self.process_id();
        call(self.client.start_process(&StartProcessParams {
            process_id: process_id.clone(),
            command: shell(script, input, self.work),
            pty: None,
        }))
        .await?;
        Ok(process_id)
    }

    /// Read from the cursor until the exit has settled and all output is in.
    async fn read_to_exit(
        &self,
        process_id: &ProcessId,
    ) -> Result<(Vec<u8>, ReadProcessResponse), String> {
        let deadline = Instant::now() + self.timeout;
        let mut output = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("the command did not exit in time".into());
            }
            let response = call(self.client.read_process(&ReadProcessParams {
                process_id: process_id.clone(),
                after: None,
                tail_bytes: None,
                ack: None,
                max_bytes: self.initialized.limits.process.max_read_bytes,
                wait_ms: (remaining.as_millis() as u64).min(self.initialized.limits.max_wait_ms),
                return_on: ReturnOn::Exit,
            }))
            .await?;
            ensure(
                response.output.gaps.is_empty(),
                "output gaps in a short command",
            )?;
            for chunk in &response.output.chunks {
                output.extend_from_slice(chunk.data.as_slice());
            }
            if response.state == ExecState::Exited && response.output.next == response.output.end {
                return Ok((output, response));
            }
        }
    }

    async fn process_exit(&self) -> Check {
        let process_id = self
            .start("printf out; printf err >&2; exit 3", ExecInput::None {})
            .await?;
        let (output, response) = self.read_to_exit(&process_id).await?;
        ensure(
            response.exit == Some(ExitStatus::Code { code: 3 }),
            format!("exit was {:?}, not code 3", response.exit),
        )?;
        let text = String::from_utf8_lossy(&output);
        ensure(
            text.contains("out") && text.contains("err"),
            "stdout and stderr both reach the output",
        )
    }

    async fn process_signal(&self) -> Check {
        let process_id = self.start("kill -TERM $$", ExecInput::None {}).await?;
        let (_, response) = self.read_to_exit(&process_id).await?;
        match &response.exit {
            Some(ExitStatus::Signal { signal, .. }) if signal.as_str() == "SIGTERM" => Ok(()),
            other => Err(format!("a signal death was reported as {other:?}")),
        }
    }

    async fn process_large_input(&self) -> Check {
        let input = vec![b'x'; 1 << 20];
        let process_id = self
            .start(
                "cat",
                ExecInput::Bytes {
                    data: ByteChunk::from(input.clone()),
                },
            )
            .await?;
        let (output, response) = self.read_to_exit(&process_id).await?;
        ensure(
            response.output.end == input.len() as u64 && output == input,
            format!(
                "cat echoed {} of {} input bytes",
                response.output.end,
                input.len()
            ),
        )?;
        let tail = self
            .client
            .read_process(&ReadProcessParams {
                process_id,
                after: None,
                tail_bytes: Some(10),
                ack: None,
                max_bytes: 10,
                wait_ms: 0,
                return_on: ReturnOn::AnyOutput,
            })
            .await;
        match tail {
            // A fully read process may already be released.
            Err(error) if error.protocol_kind() == Some(ProtocolErrorKind::NotFound) => Ok(()),
            Err(error) => Err(error.to_string()),
            Ok(tail) => ensure(
                tail.output.next == tail.output.end
                    && tail
                        .output
                        .chunks
                        .first()
                        .is_none_or(|chunk| chunk.offset >= tail.output.end - 10),
                "a tail read starts at end - tailBytes",
            ),
        }
    }

    async fn process_duplicate_id(&self) -> Check {
        let marker = self.path("duplicate-marker")?;
        let script = format!("echo ran >> '{marker}'");
        let process_id = self.start("sleep 30", ExecInput::None {}).await?;
        let duplicate = self
            .client
            .start_process(&StartProcessParams {
                process_id: process_id.clone(),
                command: shell(&script, ExecInput::None {}, self.work),
                pty: None,
            })
            .await;
        let _ = self
            .client
            .terminate_process(&TerminateProcessParams {
                process_id,
                signal: ProcessSignal::Kill,
            })
            .await;
        expect_kind(duplicate, ProtocolErrorKind::AlreadyExists)?;
        expect_kind(
            self.client
                .read_file(&ReadFileParams {
                    path: marker,
                    offset: 0,
                    max_bytes: 16,
                })
                .await,
            ProtocolErrorKind::NotFound,
        )
        .map_err(|_| "a refused duplicate start ran its command".to_owned())
    }

    async fn process_unknown(&self) -> Check {
        let unknown = self.process_id();
        expect_kind(
            self.client
                .read_process(&ReadProcessParams {
                    process_id: unknown.clone(),
                    after: None,
                    tail_bytes: None,
                    ack: None,
                    max_bytes: 1,
                    wait_ms: 0,
                    return_on: ReturnOn::AnyOutput,
                })
                .await,
            ProtocolErrorKind::NotFound,
        )?;
        expect_kind(
            self.client
                .write_process(&WriteProcessParams {
                    process_id: unknown.clone(),
                    data: ByteChunk::from(&b"x"[..]),
                    close_input: false,
                })
                .await,
            ProtocolErrorKind::NotFound,
        )?;
        expect_kind(
            self.client
                .terminate_process(&TerminateProcessParams {
                    process_id: unknown,
                    signal: ProcessSignal::Kill,
                })
                .await,
            ProtocolErrorKind::NotFound,
        )
    }

    async fn process_terminate(&self) -> Check {
        let process_id = self.start("sleep 60", ExecInput::None {}).await?;
        let response = call(self.client.terminate_process(&TerminateProcessParams {
            process_id: process_id.clone(),
            signal: ProcessSignal::Terminate,
        }))
        .await?;
        ensure(
            response.running,
            "terminate of a running process reported it not running",
        )?;
        let (_, exited) = self.read_to_exit(&process_id).await?;
        ensure(
            exited.stopped_by == Some(StoppedBy::Terminate),
            format!("stoppedBy was {:?}", exited.stopped_by),
        )
    }

    fn job(&self, job_id: &JobId, script: &str) -> JobSpec {
        JobSpec {
            job_id: job_id.clone(),
            name: None,
            command: shell(script, ExecInput::None {}, self.work),
            depends_on: Vec::new(),
            dependency_policy: Default::default(),
            queue_key: None,
        }
    }

    fn job_ids(&self) -> (JobId, JobRequestId, JobRequestId) {
        let suffix = uuid::Uuid::new_v4().simple();
        (
            JobId::new(format!("conformance-{suffix}")).expect("job id"),
            JobRequestId::new(format!("request-a-{suffix}")).expect("request id"),
            JobRequestId::new(format!("request-b-{suffix}")).expect("request id"),
        )
    }

    async fn job_request_id(&self) -> Check {
        let (job_id, first, second) = self.job_ids();
        let start = |request_id: &JobRequestId| StartJobsParams {
            request_id: request_id.clone(),
            jobs: vec![self.job(&job_id, "true")],
        };
        let started = call(self.client.start_jobs(&start(&first))).await?;
        let replayed = call(self.client.start_jobs(&start(&first))).await?;
        ensure(
            started.jobs[0].created_at_ms == replayed.jobs[0].created_at_ms,
            "a retried start with the same requestId created another run",
        )?;
        expect_precondition(
            self.client.start_jobs(&start(&second)).await,
            PreconditionReason::JobIdInUse,
        )
    }

    async fn job_unknown(&self) -> Check {
        let (job_id, _, _) = self.job_ids();
        let response = call(self.client.read_jobs(&ReadJobsParams {
            jobs: vec![JobReadRequest {
                job_id,
                after: None,
                tail_bytes: None,
                max_bytes: None,
            }],
            max_bytes: 0,
            wait_ms: 0,
            return_on: ReturnOn::AnyOutput,
        }))
        .await?;
        match response.jobs.as_slice() {
            [JobReadEntry::Error { error, .. }] if error.kind() == ProtocolErrorKind::NotFound => {
                Ok(())
            }
            other => Err(format!("an unknown job read answered {other:?}")),
        }
    }

    async fn job_paging(&self) -> Check {
        let (job_id, request_id, _) = self.job_ids();
        // 20 000 bytes: 2 000 numbered lines of 10 bytes each.
        let script = "i=0; while [ $i -lt 2000 ]; do printf '%09d\\n' $i; i=$((i+1)); done";
        call(self.client.start_jobs(&StartJobsParams {
            request_id,
            jobs: vec![self.job(&job_id, script)],
        }))
        .await?;
        let deadline = Instant::now() + self.timeout;
        let mut output = Vec::new();
        let mut after = 0;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("the job did not finish in time".into());
            }
            let response = call(self.client.read_jobs(&ReadJobsParams {
                jobs: vec![JobReadRequest {
                    job_id: job_id.clone(),
                    after: Some(after),
                    tail_bytes: None,
                    max_bytes: None,
                }],
                max_bytes: 4096,
                wait_ms: (remaining.as_millis() as u64).min(1000),
                return_on: ReturnOn::Window,
            }))
            .await?;
            let [
                JobReadEntry::Job {
                    summary,
                    output: page,
                },
            ] = response.jobs.as_slice()
            else {
                return Err(format!("job read answered {:?}", response.jobs));
            };
            ensure(page.gaps.is_empty(), "a 20 000-byte job reported gaps")?;
            for chunk in &page.chunks {
                ensure(
                    chunk.offset == output.len() as u64,
                    "pages skipped or repeated bytes",
                )?;
                output.extend_from_slice(chunk.data.as_slice());
            }
            after = page.next;
            if summary.status.is_terminal() && page.next == page.end {
                break;
            }
        }
        let expected = (0..2000).map(|i| format!("{i:09}\n")).collect::<String>();
        ensure(
            output == expected.as_bytes(),
            format!(
                "paged output was {} bytes, not 20 000 exact bytes",
                output.len()
            ),
        )
    }
}

fn shell(script: &str, input: ExecInput, cwd: &EnvironmentPath) -> CommandSpec {
    CommandSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: Some(cwd.clone()),
        env: BTreeMap::new(),
        secret_env: BTreeMap::new(),
        input,
        timeout_ms: Some(120_000),
    }
}

fn join(base: &EnvironmentPath, name: &str) -> Result<EnvironmentPath, String> {
    let tree = TreePath::new(name).map_err(|error| error.to_string())?;
    base.join(&tree).map_err(|error| error.to_string())
}

fn ensure(condition: bool, message: impl Into<String>) -> Check {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

async fn call<T>(future: impl Future<Output = Result<T, ClientError>>) -> Result<T, String> {
    future.await.map_err(|error| error.to_string())
}

fn expect_kind<T: fmt::Debug>(result: Result<T, ClientError>, kind: ProtocolErrorKind) -> Check {
    match result {
        Err(error) if error.protocol_kind() == Some(kind) => Ok(()),
        other => Err(format!("expected {kind}, got {other:?}")),
    }
}

fn expect_precondition<T: fmt::Debug>(
    result: Result<T, ClientError>,
    reason: PreconditionReason,
) -> Check {
    match &result {
        Err(error)
            if matches!(
                error.protocol_error().map(|error| &error.data),
                Some(ErrorData::FailedPrecondition { reason: actual }) if *actual == reason
            ) =>
        {
            Ok(())
        }
        other => Err(format!(
            "expected failedPrecondition ({reason:?}), got {other:?}"
        )),
    }
}
