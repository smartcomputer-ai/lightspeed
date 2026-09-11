//! Bounded endpoint-local observations through the shared scoped filesystem backend.
use super::backend::{
    self, Directory, Result, conflict, digest, error, invalid, io, relative, valid_path,
};
use environment_protocol::shared::EnvironmentPath;
use environment_protocol::{
    data::inventory::*, error::EnvironmentProtocolErrorCode as Code, shared::ByteChunk,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::PathBuf};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

pub fn scan(root: &Path, params: ScanParams) -> Result<ScanResponse> {
    let ceiling = InventoryLimits::default();
    let limits = params.limits;
    if limits.max_entries == 0
        || limits.max_entries > ceiling.max_entries
        || limits.max_depth > ceiling.max_depth
        || limits.max_file_bytes > ceiling.max_file_bytes
        || limits.max_total_bytes > ceiling.max_total_bytes
        || limits.max_manifest_bytes > ceiling.max_manifest_bytes
        || limits.max_duration_ms == 0
        || limits.max_duration_ms > ceiling.max_duration_ms
        || params.roots.len() > 32
        || params.include_patterns.len() > 32
        || params
            .include_patterns
            .iter()
            .any(|p| p.len() > MAX_INVENTORY_PATH_BYTES)
    {
        return Err(invalid("scan quotas exceed daemon ceilings"));
    }
    let patterns = params
        .include_patterns
        .iter()
        .map(|p| glob::Pattern::new(p).map_err(|e| invalid(&e.to_string())))
        .collect::<Result<Vec<_>>>()?;
    // Filename-only patterns select direct children, so unrelated subtrees do
    // not consume scan budgets or cause failures for a nonrecursive query.
    let direct_only = !params.include_patterns.is_empty()
        && params
            .include_patterns
            .iter()
            .all(|pattern| !pattern.contains('/') && !pattern.contains("**"));
    let canonical_root = root.canonicalize().map_err(io)?;
    let anchor = Directory::anchor(&canonical_root).map_err(io)?;
    let mut response = ScanResponse {
        fingerprint: None,
        unchanged: false,
        complete: true,
        entries: vec![],
        diagnostics: vec![],
    };
    let start = Instant::now();
    let mut visited = 0u32;
    let mut inspected = 0u64;
    let mut response_bytes = 0u64;
    let mut observations = Vec::new();
    let mut observation_bytes = 0u64;
    for selected in &params.roots {
        let result = (|| -> Result<()> {
            let selected_relative = relative(root, selected)?;
            let selected_path = canonical_root.join(selected_relative);
            // A missing configured root is a complete empty observation. A dangling
            // link or a permission error is an incomplete observation.
            match std::fs::symlink_metadata(&selected_path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let mut ancestor = selected_path.parent();
                    while let Some(path) = ancestor {
                        match std::fs::symlink_metadata(path) {
                            Ok(_) => {
                                let resolved = path.canonicalize().map_err(io)?;
                                if !resolved.starts_with(&canonical_root) {
                                    return Err(error(
                                        Code::Forbidden,
                                        "root outside access scope",
                                    ));
                                }
                                break;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                ancestor = path.parent()
                            }
                            Err(e) => return Err(io(e)),
                        }
                    }
                    return Ok(());
                }
                Err(e) => return Err(io(e)),
                Ok(_) => {}
            }
            let mut pending = vec![(selected_path, String::new(), BTreeSet::<PathBuf>::new())];
            while let Some((requested, path, mut ancestors)) = pending.pop() {
                visited += 1;
                if visited > limits.max_entries {
                    return Err(invalid("scan visited entry limit exceeded"));
                }
                if start.elapsed() > Duration::from_millis(limits.max_duration_ms.min(30_000)) {
                    return Err(error(Code::Timeout, "scan deadline exceeded"));
                }
                if !valid_path(&path)
                    || (!path.is_empty() && path.split('/').count() > limits.max_depth as usize)
                {
                    return Err(invalid("scan depth/path limit exceeded"));
                }
                let canonical = requested.canonicalize().map_err(io)?;
                let scoped = canonical
                    .strip_prefix(&canonical_root)
                    .map_err(|_| error(Code::Forbidden, "symlink target outside access scope"))?;
                if !params.follow_symlinks && requested != canonical {
                    return Err(error(Code::Forbidden, "scan symlink following disabled"));
                }
                let scoped = scoped.to_str().ok_or_else(|| invalid("non-UTF-8 path"))?;
                // The canonical spelling is still opened beneath an anchored directory
                // without following symlinks, preventing a retarget race from escaping.
                let mut file = if !params.read_content && params.digest_algorithm.is_none() {
                    anchor.metadata(scoped)
                } else {
                    anchor.open(scoped)
                }
                .map_err(io)?;
                let observed = backend::observe(&file).map_err(io)?;
                let matches = patterns.is_empty() || patterns.iter().any(|p| p.matches(&path));
                let mut data = None;
                let content = if observed.is_dir() {
                    if direct_only && !path.is_empty() {
                        continue;
                    }
                    if !ancestors.insert(canonical.clone()) {
                        return Err(error(Code::Conflict, "scan symlink loop"));
                    }
                    let names = Directory::names(&file, limits.max_entries as usize).map_err(io)?;
                    if visited as usize + pending.len() + names.len() > limits.max_entries as usize
                    {
                        return Err(invalid("scan traversal limit exceeded"));
                    }
                    let pending_bytes: usize = pending
                        .iter()
                        .map(|(requested, path, ancestors)| {
                            requested.as_os_str().len()
                                + path.len()
                                + ancestors.iter().map(|p| p.as_os_str().len()).sum::<usize>()
                        })
                        .sum();
                    let added_bytes: usize = names
                        .iter()
                        .map(|name| {
                            requested.as_os_str().len()
                                + path.len()
                                + name.len() * 2
                                + ancestors.iter().map(|p| p.as_os_str().len()).sum::<usize>()
                        })
                        .sum();
                    if (pending_bytes + added_bytes) as u64 > limits.max_manifest_bytes {
                        return Err(invalid("scan traversal byte limit exceeded"));
                    }
                    for name in names.into_iter().rev() {
                        if !valid_path(&name) {
                            return Err(invalid("unsupported filename"));
                        }
                        let child = if path.is_empty() {
                            name.clone()
                        } else {
                            format!("{path}/{name}")
                        };
                        pending.push((requested.join(name), child, ancestors.clone()));
                    }
                    ScanContent::Directory
                } else {
                    let digest =
                        if matches && (params.read_content || params.digest_algorithm.is_some()) {
                            if observed.size() > limits.max_file_bytes {
                                return Err(invalid("scan per-file byte limit exceeded"));
                            }
                            inspected = inspected
                                .checked_add(observed.size())
                                .ok_or_else(|| invalid("byte overflow"))?;
                            if inspected > limits.max_total_bytes {
                                return Err(invalid("scan total byte limit exceeded"));
                            }
                            if params.read_content && observed.size() > MAX_CONTENT_CHUNK as u64 {
                                return Err(invalid("scan response file limit exceeded"));
                            }
                            let mut hash = Sha256::new();
                            let mut bytes = Vec::new();
                            let mut read = 0u64;
                            let mut buffer = [0u8; 65536];
                            loop {
                                let count = file.read(&mut buffer).map_err(io)?;
                                if count == 0 {
                                    break;
                                }
                                read += count as u64;
                                if read > observed.size() {
                                    return Err(conflict());
                                }
                                if start.elapsed()
                                    > Duration::from_millis(limits.max_duration_ms.min(30_000))
                                {
                                    return Err(error(Code::Timeout, "scan deadline exceeded"));
                                }
                                hash.update(&buffer[..count]);
                                if params.read_content {
                                    bytes.extend_from_slice(&buffer[..count]);
                                }
                            }
                            if read != observed.size()
                                || !observed.matches(&backend::observe(&file).map_err(io)?)
                            {
                                return Err(conflict());
                            }
                            if params.read_content {
                                data = Some(ByteChunk::from(bytes));
                            }
                            params.digest_algorithm.map(|_| digest(hash))
                        } else {
                            None
                        };
                    ScanContent::File {
                        size_bytes: observed.size(),
                        executable: observed.executable(),
                        digest,
                    }
                };
                observation_bytes +=
                    (requested.as_os_str().len() + canonical.as_os_str().len() + 128) as u64;
                if observation_bytes > limits.max_manifest_bytes {
                    return Err(invalid("scan observation memory limit exceeded"));
                }
                observations.push((requested, canonical.clone(), observed));
                if matches {
                    let row = ScanEntry {
                        root: selected.clone(),
                        path,
                        canonical_path: EnvironmentPath::new(canonical.to_string_lossy())
                            .map_err(|e| invalid(&e.to_string()))?,
                        content,
                        data,
                    };
                    response_bytes += serde_json::to_vec(&row)
                        .map_err(|e| invalid(&e.to_string()))?
                        .len() as u64;
                    if response_bytes > limits.max_manifest_bytes.min(4 * 1024 * 1024)
                        || response.entries.len() >= 10_000
                    {
                        return Err(invalid("scan response byte limit exceeded"));
                    }
                    response.entries.push(row);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            response.complete = false;
            response.diagnostics.push(ScanDiagnostic {
                root: selected.clone(),
                error,
            });
        }
    }
    // Validate identities and observations again, including nonmatching directories.
    for (requested, canonical, observed) in observations {
        let result = (|| -> Result<()> {
            if start.elapsed() > Duration::from_millis(limits.max_duration_ms.min(30_000)) {
                return Err(error(Code::Timeout, "scan deadline exceeded"));
            }
            if requested.canonicalize().map_err(io)? != canonical {
                return Err(conflict());
            }
            let path = canonical
                .strip_prefix(&canonical_root)
                .unwrap()
                .to_str()
                .ok_or_else(|| invalid("non-UTF-8 path"))?;
            if !observed
                .matches(&backend::observe(&anchor.metadata(path).map_err(io)?).map_err(io)?)
            {
                return Err(conflict());
            }
            Ok(())
        })();
        if let Err(error) = result {
            response.complete = false;
            response.diagnostics.push(ScanDiagnostic {
                root: EnvironmentPath::new(requested.to_string_lossy())
                    .map_err(|e| invalid(&e.to_string()))?,
                error,
            });
        }
    }
    if response.complete {
        response
            .entries
            .sort_by(|a, b| (a.root.as_str(), &a.path).cmp(&(b.root.as_str(), &b.path)));
        let mut query = params.clone();
        query.if_none_match = None;
        let fingerprint = digest(Sha256::new_with_prefix(
            serde_json::to_vec(&(&canonical_root, query, &response.entries))
                .map_err(|e| invalid(&e.to_string()))?,
        ));
        response.unchanged = params.if_none_match.as_ref() == Some(&fingerprint);
        response.fingerprint = Some(fingerprint);
        if response.unchanged {
            response.entries.clear();
        }
    }
    Ok(response)
}
