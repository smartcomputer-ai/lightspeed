//! Logical filesystem paths.

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as SerdeError};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FsPath {
    normalized: String,
}

impl FsPath {
    pub fn new(path: impl AsRef<str>) -> Result<Self, FsPathError> {
        normalize_path(path.as_ref()).map(|normalized| Self { normalized })
    }

    pub fn root() -> Self {
        Self {
            normalized: "/".to_string(),
        }
    }

    pub fn current_dir() -> Self {
        Self {
            normalized: ".".to_string(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    pub fn is_absolute(&self) -> bool {
        self.normalized.starts_with('/')
    }

    pub fn is_relative(&self) -> bool {
        !self.is_absolute()
    }

    pub fn is_root(&self) -> bool {
        self.normalized == "/" || self.normalized == "."
    }

    pub fn has_unresolved_parent(&self) -> bool {
        self.segments().any(|segment| segment == "..")
    }

    pub fn join(&self, path: impl AsRef<str>) -> Result<Self, FsPathError> {
        let path = Self::new(path)?;
        self.join_path(&path)
    }

    pub fn join_path(&self, path: &Self) -> Result<Self, FsPathError> {
        if path.is_absolute() {
            return Ok(path.clone());
        }
        self.join_segments(path.segments())
    }

    pub fn join_segments<'a>(
        &self,
        segments: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, FsPathError> {
        let suffix = segments
            .into_iter()
            .filter(|segment| *segment != ".")
            .collect::<Vec<_>>();
        if suffix.is_empty() {
            return Ok(self.clone());
        }

        let mut value = self.normalized.clone();
        if value == "." {
            value = suffix.join("/");
        } else if value == "/" {
            value.push_str(&suffix.join("/"));
        } else {
            value.push('/');
            value.push_str(&suffix.join("/"));
        }
        Self::new(value)
    }

    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }

        let prefix = if self.is_absolute() { "/" } else { "" };
        let mut segments = self.segments().collect::<Vec<_>>();
        segments.pop()?;
        if segments.is_empty() {
            return Some(if self.is_absolute() {
                Self::root()
            } else {
                Self::current_dir()
            });
        }
        Self::new(format!("{prefix}{}", segments.join("/"))).ok()
    }

    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            return None;
        }
        self.normalized.rsplit('/').next()
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.normalized
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
    }

    pub fn starts_with(&self, base: &Self) -> bool {
        if base.normalized == "/" {
            return self.is_absolute();
        }
        if base.normalized == "." {
            return self.is_relative();
        }
        self.is_absolute() == base.is_absolute()
            && (self.normalized == base.normalized
                || self
                    .normalized
                    .strip_prefix(base.as_str())
                    .is_some_and(|suffix| suffix.starts_with('/')))
    }

    pub fn to_path_buf(&self) -> PathBuf {
        if self.normalized == "/" {
            return PathBuf::from("/");
        }

        let mut path = if self.is_absolute() {
            PathBuf::from("/")
        } else {
            PathBuf::new()
        };
        for segment in self.segments() {
            path.push(segment);
        }
        path
    }

    pub fn to_relative_path_buf(&self) -> PathBuf {
        let mut path = PathBuf::new();
        for segment in self.segments() {
            path.push(segment);
        }
        path
    }
}

impl fmt::Display for FsPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.normalized.fmt(formatter)
    }
}

impl AsRef<str> for FsPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for FsPath {
    type Err = FsPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for FsPath {
    type Error = FsPathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for FsPath {
    type Error = FsPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for FsPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FsPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(SerdeError::custom)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FsPathError {
    #[error("filesystem path is empty")]
    Empty,

    #[error("filesystem path contains a NUL byte")]
    NulByte,

    #[error("filesystem path must use '/' separators: {path}")]
    NonUnixSeparator { path: String },
}

fn normalize_path(path: &str) -> Result<String, FsPathError> {
    if path.is_empty() {
        return Err(FsPathError::Empty);
    }
    if path.contains('\0') {
        return Err(FsPathError::NulByte);
    }
    if path.contains('\\') {
        return Err(FsPathError::NonUnixSeparator {
            path: path.to_string(),
        });
    }

    let absolute = path.starts_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = segments.last()
                    && *last != ".."
                {
                    segments.pop();
                    continue;
                }
                if !absolute {
                    segments.push("..");
                }
            }
            segment => segments.push(segment),
        }
    }

    if absolute {
        if segments.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", segments.join("/")))
        }
    } else if segments.is_empty() {
        Ok(".".to_string())
    } else {
        Ok(segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_normalizes_relative_path() {
        let path = FsPath::new("src//./lib/../main.rs").expect("valid path");

        assert_eq!(path.as_str(), "src/main.rs");
    }

    #[test]
    fn new_accepts_absolute_path() {
        let path = FsPath::new("/tmp/../var/log").expect("absolute path");

        assert_eq!(path.as_str(), "/var/log");
        assert!(path.is_absolute());
    }

    #[test]
    fn new_preserves_leading_relative_parent() {
        let path = FsPath::new("../outside/file").expect("relative parent path");

        assert_eq!(path.as_str(), "../outside/file");
        assert!(path.has_unresolved_parent());
    }

    #[test]
    fn starts_with_respects_path_boundaries() {
        let base = FsPath::new("/tmp/work").expect("base path");

        assert!(FsPath::new("/tmp/work/file").unwrap().starts_with(&base));
        assert!(
            !FsPath::new("/tmp/workspace/file")
                .unwrap()
                .starts_with(&base)
        );
    }
}
