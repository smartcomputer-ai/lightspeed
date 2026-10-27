//! Shared machine source scope, resolved from the session's effective directory.
use environment_protocol::{
    data::{
        fs::TreeFilter,
        scan::{ScanLimits, ScanParams, ScanRoot},
    },
    paths::EnvironmentPath,
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
    // One root slot of the protocol's 32 stays free for the working
    // directory probe.
    if roots.len() > 31 {
        return Err("discovery exceeds 31 roots".into());
    }
    // Discovery roots are often hidden (`.agents`) or ignored by a project's
    // ignore files, so both defaults are opted out of below each root.
    Ok(ScanParams {
        roots: roots
            .into_iter()
            .map(|path| {
                Ok(ScanRoot {
                    path: EnvironmentPath::new(path.to_string_lossy())
                        .map_err(|e| e.to_string())?,
                    if_none_match: None,
                    max_depth: None,
                })
            })
            .collect::<Result<_, String>>()?,
        filter: TreeFilter {
            include: if source == "prompts" {
                vec!["*.md".into(), "*.txt".into()]
            } else {
                vec!["SKILL.md".into()]
            },
            include_hidden: true,
            include_ignored: true,
            max_depth: Some(if source == "prompts" { 1 } else { 8 }),
            ..TreeFilter::default()
        },
        read_content: true,
        include_digests: false,
        follow_symlinks: true,
        limits: ScanLimits {
            max_entries: 4096,
            max_file_bytes: 64 * 1024,
            max_total_bytes: 2 * 1024 * 1024,
            max_duration_ms: 2000,
        },
    })
}

/// Add the working-directory probe to a discovery scan: a root examined at
/// depth 0, so the same observation proves `cwd` is an existing directory
/// (`complete` without entries) instead of a separate `fs/getMetadata`.
/// The probe is the last root of the response.
pub fn with_working_directory_probe(params: &mut ScanParams, cwd: &EnvironmentPath) {
    params.roots.push(ScanRoot {
        path: cwd.clone(),
        if_none_match: None,
        max_depth: Some(0),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_scans_select_by_name_and_probe_the_working_directory() {
        let mut params = scan_query(None, "/project", Some("/home/dev"), "skills").unwrap();
        assert_eq!(params.filter.include, vec!["SKILL.md"]);
        assert!(params.filter.include_hidden && params.filter.include_ignored);
        let cwd = EnvironmentPath::new("/project").unwrap();
        with_working_directory_probe(&mut params, &cwd);
        let probe = params.roots.last().unwrap();
        assert_eq!(
            (probe.path.as_str(), probe.max_depth),
            ("/project", Some(0))
        );
        let prompts = scan_query(None, "/project", Some("/home/dev"), "prompts").unwrap();
        assert_eq!(prompts.filter.max_depth, Some(1));
    }
}
