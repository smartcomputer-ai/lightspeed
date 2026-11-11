//! Shared machine source scope, resolved from the session's effective directory.
use environment_protocol::{
    data::{
        fs::TreeFilter,
        scan::{ScanLimits, ScanParams, ScanRoot},
    },
    paths::EnvironmentPath,
};
use std::collections::BTreeSet;

pub fn scan_query(
    overrides: Option<&[String]>,
    cwd: &str,
    home: Option<&str>,
    source: &str,
) -> Result<ScanParams, String> {
    let cwd = EnvironmentPath::new(cwd).map_err(|error| error.to_string())?;
    let mut roots = BTreeSet::new();
    if let Some(overrides) = overrides {
        if overrides.is_empty() {
            return Err("source root overrides must not be empty".into());
        }
        for root in overrides {
            roots.insert(
                environment_client::paths::resolve(&cwd, root)
                    .map_err(|error| error.to_string())?,
            );
        }
    } else {
        let home = home.ok_or("endpoint does not advertise execution user home directory")?;
        let home = environment_client::paths::absolute(home, cwd.convention())
            .map_err(|error| error.to_string())?;
        for base in [&cwd, &home] {
            for prefix in [".agents", ".lightspeed"] {
                roots.insert(
                    environment_client::paths::resolve(base, &format!("{prefix}/{source}"))
                        .map_err(|error| error.to_string())?,
                );
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
            .map(|path| ScanRoot {
                path,
                if_none_match: None,
                max_depth: None,
            })
            .collect(),
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
    fn windows_source_roots_are_resolved_independently_of_controller_os() {
        let query = scan_query(None, "C:/work/project", Some(r"D:\Users\dev"), "skills").unwrap();
        let roots: Vec<_> = query.roots.iter().map(|root| root.path.as_str()).collect();
        assert_eq!(
            roots,
            [
                "C:/work/project/.agents/skills",
                "C:/work/project/.lightspeed/skills",
                "D:/Users/dev/.agents/skills",
                "D:/Users/dev/.lightspeed/skills"
            ]
        );
        let query = scan_query(
            Some(&[r"..\shared".into(), r"E:\skills".into()]),
            "C:/work/project",
            None,
            "skills",
        )
        .unwrap();
        assert_eq!(query.roots[0].path.as_str(), "C:/work/shared");
        assert_eq!(query.roots[1].path.as_str(), "E:/skills");
        assert!(scan_query(Some(&[r"\rooted".into()]), "C:/work", None, "skills").is_err());
    }

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
