//! Resolve user paths using the remote grammar, independently of the host OS.
use environment_protocol::paths::{EnvironmentPath, PathConvention, PathError};

pub fn resolve(base: &EnvironmentPath, value: &str) -> Result<EnvironmentPath, PathError> {
    if value.is_empty() {
        return Err(PathError::Empty);
    }
    let windows = base.convention() == PathConvention::Windows;
    let value = if windows {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    };
    let drive = value.as_bytes().get(1) == Some(&b':');
    let absolute = if windows {
        drive
    } else {
        value.starts_with('/')
    };
    if windows
        && (value.starts_with('/')
            || (drive
                && (value.as_bytes().get(2) != Some(&b'/')
                    || !value.as_bytes()[0].is_ascii_alphabetic())))
    {
        return Err(PathError::UnsupportedWindowsPath { path: value });
    }
    let combined = if absolute {
        value
    } else {
        format!("{}/{value}", base.as_str().trim_end_matches('/'))
    };
    let root_len = if windows { 3 } else { 1 };
    let mut root = combined[..root_len].to_owned();
    if windows {
        root[..1].make_ascii_uppercase();
    }
    let mut segments = Vec::new();
    for segment in combined[root_len..].split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(PathError::NotNormalized { path: combined });
                }
            }
            name => {
                // Validate before normalization so an invalid name cannot be
                // hidden by a later parent segment.
                EnvironmentPath::new(format!("{root}{name}"))?;
                segments.push(name);
            }
        }
    }
    EnvironmentPath::from_native(&format!("{root}{}", segments.join("/")), base.convention())
}

pub fn absolute(value: &str, convention: PathConvention) -> Result<EnvironmentPath, PathError> {
    let root = match convention {
        PathConvention::Posix if value.starts_with('/') => "/".to_owned(),
        PathConvention::Windows
            if value.len() >= 3
                && value.as_bytes()[0].is_ascii_alphabetic()
                && value.as_bytes()[1] == b':'
                && matches!(value.as_bytes()[2], b'/' | b'\\') =>
        {
            format!("{}:/", value[..1].to_ascii_uppercase())
        }
        _ => return Err(PathError::NotAbsolute { path: value.into() }),
    };
    resolve(&EnvironmentPath::new(root)?, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_resolution_does_not_use_host_path_rules() {
        let base = EnvironmentPath::new("C:/work/src").unwrap();
        assert_eq!(
            resolve(&base, r"..\雪\file.txt").unwrap().as_str(),
            "C:/work/雪/file.txt"
        );
        assert_eq!(
            resolve(&base, "d:/other/./a/..").unwrap().as_str(),
            "D:/other"
        );
        for path in [
            r"C:relative",
            r"\rooted",
            r"\\server\share",
            r"\\?\C:\work",
            "NUL/../x",
            "C:/../x",
            "file:stream",
        ] {
            assert!(resolve(&base, path).is_err(), "{path}");
        }
        assert_eq!(
            resolve(&EnvironmentPath::new("/work").unwrap(), r"a\b")
                .unwrap()
                .as_str(),
            r"/work/a\b"
        );
    }
}
