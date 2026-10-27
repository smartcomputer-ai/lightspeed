//! `fs/scan`: a bounded observation of several roots, each reported on its
//! own. A skipped entry never fails its root, and a failed root never hides
//! the others.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use environment_protocol::{
    bytes::ByteChunk,
    data::{
        fs::TreeContent,
        handshake::ScanCeilings,
        scan::{
            ScanEntry, ScanFingerprint, ScanParams, ScanResponse, ScanRoot, ScanRootResult,
            ScanSkip, ScanSkipReason,
        },
    },
    error::{ErrorData, PreconditionReason, ProtocolError},
    limits::{MAX_LISTED_SKIPS, MAX_RESULT_BYTES},
    paths::{ContentDigest, TreePath},
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{
    confinement::{Confinement, Lookup},
    json_string_len,
    os::{self, EntryStat},
    search::check_quota,
    walk::{CompiledFilter, Event, LinkMode, Skip, Walker},
};

/// Encoded overhead of one entry or skip besides its path and content.
const ENTRY_OVERHEAD: u64 = 96;

pub(crate) fn check_scan(
    params: &ScanParams,
    ceilings: &ScanCeilings,
) -> Result<(), ProtocolError> {
    let roots = params.roots.len() as u64;
    if roots == 0 {
        return Err(ProtocolError::invalid_field(
            "roots",
            "scan needs at least one root",
        ));
    }
    if roots > u64::from(ceilings.max_roots) {
        return Err(ProtocolError::above_ceiling(
            "roots",
            roots,
            ceilings.max_roots.into(),
        ));
    }
    let patterns = (params.filter.include.len() + params.filter.exclude.len()) as u64;
    if patterns > u64::from(ceilings.max_patterns) {
        return Err(ProtocolError::above_ceiling(
            "filter",
            patterns,
            ceilings.max_patterns.into(),
        ));
    }
    let limits = &params.limits;
    check_quota(
        "limits.maxEntries",
        limits.max_entries.into(),
        ceilings.max_entries.into(),
    )?;
    if limits.max_file_bytes > ceilings.max_file_bytes {
        return Err(ProtocolError::above_ceiling(
            "limits.maxFileBytes",
            limits.max_file_bytes,
            ceilings.max_file_bytes,
        ));
    }
    if limits.max_total_bytes > ceilings.max_total_bytes {
        return Err(ProtocolError::above_ceiling(
            "limits.maxTotalBytes",
            limits.max_total_bytes,
            ceilings.max_total_bytes,
        ));
    }
    check_quota(
        "limits.maxDurationMs",
        limits.max_duration_ms,
        ceilings.max_duration_ms,
    )
}

/// Budgets shared by every root of one scan.
struct Totals {
    content_bytes: u64,
    result_bytes: u64,
}

pub(crate) fn scan(
    confinement: &Arc<Confinement>,
    params: ScanParams,
    cancel: &CancellationToken,
) -> Result<ScanResponse, ProtocolError> {
    let filter = CompiledFilter::compile(&params.filter, "filter")?;
    let mut totals = Totals {
        content_bytes: 0,
        result_bytes: 0,
    };
    let mut roots = Vec::with_capacity(params.roots.len());
    for root in &params.roots {
        let result = match scan_root(confinement, &params, root, &filter, &mut totals, cancel) {
            Ok(result) => result,
            Err(RootError::Cancelled) => return Err(ProtocolError::cancelled()),
            Err(RootError::Failed(error)) => ScanRootResult::Failed {
                path: root.path.clone(),
                error,
            },
        };
        roots.push(result);
    }
    Ok(ScanResponse { roots })
}

enum RootError {
    Cancelled,
    Failed(ProtocolError),
}

impl From<ProtocolError> for RootError {
    fn from(error: ProtocolError) -> Self {
        Self::Failed(error)
    }
}

fn scan_root(
    confinement: &Arc<Confinement>,
    params: &ScanParams,
    root: &ScanRoot,
    filter: &CompiledFilter,
    totals: &mut Totals,
    cancel: &CancellationToken,
) -> Result<ScanRootResult, RootError> {
    let started = Instant::now();
    let deadline = Duration::from_millis(params.limits.max_duration_ms);
    let resolved = match confinement.lookup(&root.path, Lookup::FOLLOW) {
        Ok(resolved) => resolved,
        Err(error) if matches!(error.data, ErrorData::NotFound { .. }) => {
            return Ok(ScanRootResult::Missing {
                path: root.path.clone(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !resolved.stat.is_some_and(|stat| stat.is_dir()) {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::NotDirectory,
            format!("{} is not a directory", root.path),
        )
        .into());
    }
    let canonical_path = confinement
        .wire_path(&resolved.segments)
        .filter(|canonical| canonical != &root.path);
    let links = if params.follow_symlinks {
        LinkMode::Follow
    } else {
        LinkMode::Report
    };
    let mut walker = Walker::new(
        confinement.clone(),
        &resolved,
        filter.clone(),
        links,
        root.max_depth,
        &root.path,
    )?;
    let limits = params.limits;
    let read_files = params.read_content || params.include_digests;
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_entry_count = 0u32;
    let mut content_bytes = 0u64;
    let mut result_bytes = 0u64;
    let mut fingerprint = Sha256::new();
    fingerprint.update([
        u8::from(params.read_content),
        u8::from(params.include_digests),
    ]);
    let mut skip = |path: Option<TreePath>, reason: ScanSkipReason, result_bytes: &mut u64| {
        skipped_entry_count = skipped_entry_count.saturating_add(1);
        if (skipped.len() as u32) < MAX_LISTED_SKIPS {
            *result_bytes += path
                .as_ref()
                .map_or(0, |path| json_string_len(path.as_str()))
                + ENTRY_OVERHEAD;
            skipped.push(ScanSkip { path, reason });
        }
    };
    while let Some(event) = walker.next_event() {
        if cancel.is_cancelled() {
            return Err(RootError::Cancelled);
        }
        if started.elapsed() >= deadline {
            return Err(ProtocolError::deadline_exceeded(format!(
                "scanning {} took longer than {} ms",
                root.path, limits.max_duration_ms
            ))
            .into());
        }
        let entry = match event {
            Event::Yield
            | Event::Skip {
                selected: false, ..
            } => continue,
            Event::Skip { path, reason, .. } => {
                let reason = match reason {
                    Skip::NonUtf8Name => ScanSkipReason::NonUtf8Name,
                    Skip::Unreadable(_) | Skip::DanglingLink => ScanSkipReason::Unreadable,
                    Skip::LinkOutsideRoot(_) => ScanSkipReason::SymlinkOutsideRoot,
                };
                skip(path, reason, &mut result_bytes);
                continue;
            }
            Event::Entry(entry) => entry,
        };
        if !entry.selected {
            continue;
        }
        let (content, data) = match entry.stat.kind {
            environment_protocol::data::fs::FileKind::Directory => {
                (TreeContent::Directory {}, None)
            }
            environment_protocol::data::fs::FileKind::Symlink => {
                skip(Some(entry.path), ScanSkipReason::Symlink, &mut result_bytes);
                continue;
            }
            environment_protocol::data::fs::FileKind::Other => {
                skip(
                    Some(entry.path),
                    ScanSkipReason::SpecialFile,
                    &mut result_bytes,
                );
                continue;
            }
            environment_protocol::data::fs::FileKind::File => {
                if entry.stat.size > limits.max_file_bytes {
                    skip(
                        Some(entry.path),
                        ScanSkipReason::FileTooLarge,
                        &mut result_bytes,
                    );
                    continue;
                }
                if !read_files {
                    (
                        TreeContent::File {
                            size_bytes: entry.stat.size,
                            executable: entry.stat.executable(),
                            digest: None,
                        },
                        None,
                    )
                } else {
                    match read_file(
                        &entry.parent,
                        &entry.name,
                        &entry.stat,
                        limits.max_file_bytes,
                    ) {
                        Ok(Some(bytes)) => {
                            let digest = ContentDigest::from_sha256(Sha256::digest(&bytes).into());
                            fingerprint.update(digest.sha256());
                            let data = if params.read_content {
                                content_bytes += bytes.len() as u64;
                                if totals.content_bytes + content_bytes > limits.max_total_bytes {
                                    return Err(ProtocolError::payload_too_large(
                                        limits.max_total_bytes,
                                        format!(
                                            "the content of {} exceeds the scan's remaining maxTotalBytes",
                                            root.path
                                        ),
                                    )
                                    .into());
                                }
                                Some(ByteChunk::from(bytes))
                            } else {
                                None
                            };
                            (
                                TreeContent::File {
                                    size_bytes: entry.stat.size,
                                    executable: entry.stat.executable(),
                                    digest: params.include_digests.then_some(digest),
                                },
                                data,
                            )
                        }
                        Ok(None) => {
                            skip(
                                Some(entry.path),
                                ScanSkipReason::FileTooLarge,
                                &mut result_bytes,
                            );
                            continue;
                        }
                        Err(()) => {
                            skip(
                                Some(entry.path),
                                ScanSkipReason::Unreadable,
                                &mut result_bytes,
                            );
                            continue;
                        }
                    }
                }
            }
        };
        if entries.len() as u32 >= limits.max_entries {
            return Err(ProtocolError::too_many_entries(
                limits.max_entries,
                format!(
                    "{} selects more than {} entries",
                    root.path, limits.max_entries
                ),
            )
            .into());
        }
        fingerprint.update(entry.path.as_str().as_bytes());
        fingerprint.update([0]);
        match &content {
            TreeContent::Directory {} => fingerprint.update(b"d"),
            TreeContent::File {
                size_bytes,
                executable,
                ..
            } => {
                fingerprint.update([b'f', u8::from(*executable)]);
                fingerprint.update(size_bytes.to_le_bytes());
            }
        }
        result_bytes += json_string_len(entry.path.as_str())
            + ENTRY_OVERHEAD
            + data.as_ref().map_or(0, |data: &ByteChunk| {
                (data.as_slice().len() as u64).div_ceil(3) * 4
            });
        if totals.result_bytes + result_bytes > MAX_RESULT_BYTES {
            return Err(ProtocolError::payload_too_large(
                MAX_RESULT_BYTES,
                format!(
                    "the result of {} exceeds the scan's response budget",
                    root.path
                ),
            )
            .into());
        }
        entries.push(ScanEntry {
            path: entry.path,
            content,
            data,
        });
    }
    let fingerprint = ScanFingerprint::new(format!(
        "{}{}",
        ScanFingerprint::PREFIX,
        hex::encode(fingerprint.finalize())
    ))
    .expect("a hex digest is a valid fingerprint");
    if root.if_none_match.as_ref() == Some(&fingerprint) {
        return Ok(ScanRootResult::Unchanged {
            path: root.path.clone(),
            fingerprint,
        });
    }
    totals.content_bytes += content_bytes;
    totals.result_bytes += result_bytes;
    Ok(ScanRootResult::Complete {
        path: root.path.clone(),
        canonical_path,
        fingerprint,
        entries,
        skipped,
        skipped_entry_count,
    })
}

/// Read a selected file whole: `Ok(None)` when it grew past `max`, `Err`
/// when it vanished, changed or could not be read.
fn read_file(
    parent: &os::Directory,
    name: &std::ffi::OsStr,
    observed: &EntryStat,
    max: u64,
) -> Result<Option<Vec<u8>>, ()> {
    let mut file = parent.open_read(name).map_err(drop)?;
    let opened = EntryStat::of_file(&file).map_err(drop)?;
    if !opened.is_file() || !opened.unchanged(observed) {
        return Err(());
    }
    let bytes = os::read_up_to(&mut file, max + 1).map_err(drop)?;
    if bytes.len() as u64 > max {
        return Ok(None);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::{
        data::{fs::TreeFilter, scan::ScanLimits},
        error::ProtocolErrorKind,
        paths::EnvironmentPath,
    };

    fn limits() -> ScanLimits {
        ScanLimits {
            max_entries: 100,
            max_file_bytes: 1024,
            max_total_bytes: 4096,
            max_duration_ms: 10_000,
        }
    }

    fn root(path: &std::path::Path) -> ScanRoot {
        ScanRoot {
            path: EnvironmentPath::new(path.to_str().unwrap()).unwrap(),
            if_none_match: None,
            max_depth: None,
        }
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Arc<Confinement>) {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().canonicalize().unwrap();
        let confinement = Arc::new(Confinement::new(host.clone(), host.join(".state")).unwrap());
        (temp, host, confinement)
    }

    #[test]
    fn one_bad_entry_is_skipped_and_one_bad_root_fails_alone() {
        let (_temp, host, confinement) = fixture();
        std::fs::create_dir_all(host.join("skills/a")).unwrap();
        std::fs::create_dir_all(host.join("skills/b")).unwrap();
        std::fs::write(host.join("skills/a/SKILL.md"), "a").unwrap();
        std::fs::write(host.join("skills/b/SKILL.md"), vec![b'b'; 2048]).unwrap();
        std::fs::write(host.join("file"), "not a directory").unwrap();
        let params = ScanParams {
            roots: vec![
                root(&host.join("skills")),
                root(&host.join("missing")),
                root(&host.join("file")),
                root(&host.join(".state")),
            ],
            filter: TreeFilter {
                include: vec!["**/SKILL.md".into()],
                ..TreeFilter::default()
            },
            read_content: true,
            include_digests: false,
            follow_symlinks: false,
            limits: limits(),
        };
        let response = scan(&confinement, params, &CancellationToken::new()).unwrap();
        let ScanRootResult::Complete {
            entries,
            skipped,
            skipped_entry_count,
            ..
        } = &response.roots[0]
        else {
            panic!("{:?}", response.roots[0]);
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.as_str(), "a/SKILL.md");
        assert_eq!(entries[0].data.as_ref().unwrap().as_slice(), b"a");
        assert_eq!(
            skipped,
            &[ScanSkip {
                path: Some(TreePath::new("b/SKILL.md").unwrap()),
                reason: ScanSkipReason::FileTooLarge,
            }]
        );
        assert_eq!(*skipped_entry_count, 1);
        assert!(matches!(response.roots[1], ScanRootResult::Missing { .. }));
        let ScanRootResult::Failed { error, .. } = &response.roots[2] else {
            panic!("{:?}", response.roots[2]);
        };
        assert!(matches!(
            error.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::NotDirectory
            }
        ));
        let ScanRootResult::Failed { error, .. } = &response.roots[3] else {
            panic!("{:?}", response.roots[3]);
        };
        assert_eq!(error.kind(), ProtocolErrorKind::PermissionDenied);
    }

    #[test]
    fn depth_zero_probes_a_directory_and_fingerprints_detect_changes() {
        let (_temp, host, confinement) = fixture();
        std::fs::create_dir_all(host.join("work")).unwrap();
        std::fs::write(host.join("work/a"), "one").unwrap();
        let probe = ScanRoot {
            max_depth: Some(0),
            ..root(&host.join("work"))
        };
        let params = |roots: Vec<ScanRoot>| ScanParams {
            roots,
            filter: TreeFilter::default(),
            read_content: false,
            include_digests: true,
            follow_symlinks: false,
            limits: limits(),
        };
        let response = scan(
            &confinement,
            params(vec![probe, root(&host.join("work"))]),
            &CancellationToken::new(),
        )
        .unwrap();
        let ScanRootResult::Complete { entries, .. } = &response.roots[0] else {
            panic!("{:?}", response.roots[0]);
        };
        assert!(entries.is_empty());
        let ScanRootResult::Complete { fingerprint, .. } = &response.roots[1] else {
            panic!("{:?}", response.roots[1]);
        };
        let again = ScanRoot {
            if_none_match: Some(fingerprint.clone()),
            ..root(&host.join("work"))
        };
        let response = scan(
            &confinement,
            params(vec![again.clone()]),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(matches!(
            response.roots[0],
            ScanRootResult::Unchanged { .. }
        ));
        std::fs::write(host.join("work/a"), "two").unwrap();
        let response = scan(&confinement, params(vec![again]), &CancellationToken::new()).unwrap();
        assert!(matches!(response.roots[0], ScanRootResult::Complete { .. }));
    }
}
