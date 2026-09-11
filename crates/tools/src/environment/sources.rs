//! Shared machine source scope, resolved from the session's effective directory.
use environment_protocol::{
    data::inventory::{InventoryLimits, ScanParams},
    shared::EnvironmentPath,
};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

pub fn absolute(base: &Path, value: &str) -> Result<PathBuf, String> {
    let path = if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        base.join(value)
    };
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("path escapes root".into());
                }
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.is_absolute() {
        return Err("working directory must be absolute".into());
    }
    Ok(normalized)
}

pub fn scan_query(
    overrides: Option<&[String]>,
    cwd: &str,
    home: Option<&str>,
    source: &str,
) -> Result<ScanParams, String> {
    if !Path::new(cwd).is_absolute() {
        return Err("working directory must be absolute".into());
    }
    let cwd = absolute(Path::new("/"), cwd)?;
    let mut roots = BTreeSet::new();
    if let Some(overrides) = overrides {
        if overrides.is_empty() {
            return Err("source root overrides must not be empty".into());
        }
        for root in overrides {
            roots.insert(absolute(&cwd, root)?);
        }
    } else {
        let home = home.ok_or("endpoint does not advertise execution user home directory")?;
        if !Path::new(home).is_absolute() {
            return Err("endpoint home directory must be absolute".into());
        }
        for base in [&cwd, &absolute(Path::new("/"), home)?] {
            for prefix in [".agents", ".lightspeed"] {
                roots.insert(base.join(prefix).join(source));
            }
        }
    }
    if roots.len() > 32 {
        return Err("discovery exceeds 32 roots".into());
    }
    Ok(ScanParams {
        roots: roots
            .into_iter()
            .map(|p| EnvironmentPath::new(p.to_string_lossy()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?,
        include_patterns: if source == "prompts" {
            vec!["*.md".into(), "*.txt".into()]
        } else {
            vec!["SKILL.md".into(), "**/SKILL.md".into()]
        },
        read_content: true,
        follow_symlinks: true,
        digest_algorithm: None,
        if_none_match: None,
        limits: InventoryLimits {
            max_entries: 4096,
            max_depth: if source == "prompts" { 1 } else { 8 },
            max_file_bytes: 64 * 1024,
            max_total_bytes: 2 * 1024 * 1024,
            max_manifest_bytes: 4 * 1024 * 1024,
            max_duration_ms: 2000,
        },
    })
}
