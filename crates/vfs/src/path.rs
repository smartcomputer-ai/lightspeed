use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VfsPath(String);

impl VfsPath {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, VfsPathError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(VfsPathError::Empty);
        }
        if value.as_bytes().contains(&0) {
            return Err(VfsPathError::ContainsNul {
                path: value.to_owned(),
            });
        }
        if value == "/" {
            return Ok(Self("/".to_owned()));
        }

        let trimmed = value.strip_prefix('/').unwrap_or(value);
        if trimmed.is_empty() {
            return Err(VfsPathError::InvalidComponent {
                path: value.to_owned(),
                component: String::new(),
            });
        }

        let mut components = Vec::new();
        for component in trimmed.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(VfsPathError::InvalidComponent {
                    path: value.to_owned(),
                    component: component.to_owned(),
                });
            }
            components.push(component);
        }

        Ok(Self(format!("/{}", components.join("/"))))
    }

    pub fn root() -> Self {
        Self("/".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    pub fn depth(&self) -> usize {
        self.components().len()
    }

    pub fn components(&self) -> Vec<&str> {
        if self.is_root() {
            Vec::new()
        } else {
            self.0.trim_start_matches('/').split('/').collect()
        }
    }
}

impl fmt::Display for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for VfsPath {
    type Error = VfsPathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<String> for VfsPath {
    type Error = VfsPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum VfsPathError {
    #[error("vfs path must not be empty")]
    Empty,

    #[error("vfs path contains a NUL byte: {path}")]
    ContainsNul { path: String },

    #[error("vfs path has an invalid component '{component}': {path}")]
    InvalidComponent { path: String, component: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfs_path_normalizes_relative_and_absolute_paths() {
        assert_eq!(VfsPath::parse("foo/bar").unwrap().as_str(), "/foo/bar");
        assert_eq!(VfsPath::parse("/foo/bar").unwrap().as_str(), "/foo/bar");
        assert_eq!(VfsPath::parse("/").unwrap(), VfsPath::root());
        assert_eq!(
            VfsPath::parse("foo bar/baz.txt").unwrap().as_str(),
            "/foo bar/baz.txt"
        );
    }

    #[test]
    fn vfs_path_rejects_unsafe_components() {
        assert!(matches!(VfsPath::parse(""), Err(VfsPathError::Empty)));
        assert!(matches!(
            VfsPath::parse("foo//bar"),
            Err(VfsPathError::InvalidComponent { .. })
        ));
        assert!(matches!(
            VfsPath::parse("foo/./bar"),
            Err(VfsPathError::InvalidComponent { .. })
        ));
        assert!(matches!(
            VfsPath::parse("foo/../bar"),
            Err(VfsPathError::InvalidComponent { .. })
        ));
        assert!(matches!(
            VfsPath::parse("foo/\0/bar"),
            Err(VfsPathError::ContainsNul { .. })
        ));
    }
}
