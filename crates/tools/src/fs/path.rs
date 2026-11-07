//! Logical workspace paths and explicitly constructed remote filesystem paths.

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as SerdeError};
use thiserror::Error;

/// Uninterpreted model input. The selected filesystem resolves its grammar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct PathInput(pub String);
impl PathInput {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn presented(&self, resolved: &FsPath) -> FsPath {
        if resolved.environment.is_some() {
            resolved.clone()
        } else {
            FsPath::new(&self.0).unwrap_or_else(|_| resolved.clone())
        }
    }
}
impl AsRef<str> for PathInput {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl From<FsPath> for PathInput {
    fn from(path: FsPath) -> Self {
        Self(path.as_str().into())
    }
}
impl From<String> for PathInput {
    fn from(path: String) -> Self {
        Self(path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, schemars::JsonSchema)]
#[schemars(with = "String")]
pub struct FsPath {
    normalized: String,
    #[schemars(skip)]
    environment: Option<Box<environment_protocol::paths::EnvironmentPath>>,
}

impl FsPath {
    pub fn new(path: impl AsRef<str>) -> Result<Self, FsPathError> {
        normalize_path(path.as_ref()).map(|normalized| Self {
            normalized,
            environment: None,
        })
    }

    /// Explicit remote-path construction. `new` retains logical/VFS semantics.
    pub fn from_environment(path: environment_protocol::paths::EnvironmentPath) -> Self {
        if path.convention() == environment_protocol::paths::PathConvention::Posix {
            return Self {
                normalized: path.to_string(),
                environment: None,
            };
        }
        Self {
            normalized: path.to_string(),
            environment: Some(Box::new(path)),
        }
    }

    pub fn environment_path(&self) -> Option<&environment_protocol::paths::EnvironmentPath> {
        self.environment.as_deref()
    }

    pub fn root() -> Self {
        Self {
            normalized: "/".to_string(),
            environment: None,
        }
    }

    pub fn current_dir() -> Self {
        Self {
            normalized: ".".to_string(),
            environment: None,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    pub fn is_absolute(&self) -> bool {
        self.environment.is_some() || self.normalized.starts_with('/')
    }

    pub fn is_relative(&self) -> bool {
        !self.is_absolute()
    }

    pub fn is_root(&self) -> bool {
        self.environment.as_ref().is_some_and(|p| p.is_root())
            || self.normalized == "/"
            || self.normalized == "."
    }

    pub fn has_unresolved_parent(&self) -> bool {
        self.segments().any(|segment| segment == "..")
    }

    pub fn join(&self, path: impl AsRef<str>) -> Result<Self, FsPathError> {
        if let Some(base) = &self.environment {
            return environment_client::paths::resolve(base, path.as_ref())
                .map(Self::from_environment)
                .map_err(|e| FsPathError::Remote {
                    message: e.to_string(),
                });
        }
        let path = Self::new(path)?;
        self.join_path(&path)
    }

    pub fn join_path(&self, path: &Self) -> Result<Self, FsPathError> {
        if path.is_absolute() {
            return Ok(path.clone());
        }
        if self.environment.is_some() {
            return self.join(path.as_str());
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
        if self.environment.is_some() {
            return self.join(suffix.join("/"));
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
        if self.environment.is_some() {
            return self.join("..").ok();
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
        if let Some(path) = &self.environment {
            return base
                .environment
                .as_ref()
                .is_some_and(|base| path.relative_to(base).is_some());
        }
        if base.environment.is_some() {
            return false;
        }
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
        if let Some(path) = &self.environment {
            return PathBuf::from(path.to_native());
        }
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
        match &self.environment {
            Some(path) => path.to_native().fmt(formatter),
            None => self.normalized.fmt(formatter),
        }
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
    #[error("invalid remote path: {message}")]
    Remote { message: String },
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
    fn native_paths_require_explicit_environment_construction() {
        use environment_protocol::paths::EnvironmentPath;
        let logical = FsPath::new("C:/work").unwrap();
        assert!(logical.is_relative());
        assert!(FsPath::new(r"C:\work").is_err());
        let native = FsPath::from_environment(EnvironmentPath::new("C:/work").unwrap());
        assert!(native.is_absolute());
        assert_ne!(native, logical);
        assert_eq!(native.to_string(), r"C:\work");
        assert_eq!(native.parent().unwrap().as_str(), "C:/");
        assert!(native.parent().unwrap().parent().is_none());
        let child = native.join_segments(["src", "main.rs"]).unwrap();
        assert!(child.starts_with(&native));
        assert_eq!(child.parent().unwrap(), native.join("src").unwrap());
        assert_eq!(native.join(r"..\other").unwrap().as_str(), "C:/other");
        assert!(native.join(r"C:relative").is_err());
        assert!(!native.starts_with(&FsPath::root()));
    }

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
