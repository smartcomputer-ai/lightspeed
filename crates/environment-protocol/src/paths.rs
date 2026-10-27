//! Path and digest values.
//!
//! **Path model.** A wire path is an absolute path in the environment's own
//! filesystem namespace, the one its processes see, not a path relative to
//! the daemon's filesystem root: with the root narrowed to `/workspace`,
//! `/workspace/src` names that directory and `/etc` is outside the root.
//! The root is not a chroot.
//!
//! **Confinement.** One rule for every data-plane method, a command's `cwd`
//! included. A path that is not the filesystem root or below it is refused
//! with `permissionDenied` (reason `outsideRoot`). Otherwise the daemon
//! strips the root prefix and resolves the remaining segments one by one
//! beneath the root's directory descriptor. A symlink met on the way is
//! followed only while its target stays inside the root; a link that leaves
//! it is refused the same way. No method addresses the daemon's state
//! directory: a path that names it, lies inside it or (for removes, renames,
//! writes and captures) contains it is refused with `permissionDenied`
//! (reason `daemonState`), and walks prune it.
//!
//! **Names.** Paths are UTF-8 and POSIX: `/` is the only separator, and
//! every other character except NUL, the backslash included, may appear in
//! a name. An entry whose name is not UTF-8 is skipped and counted wherever
//! a method enumerates entries, never returned lossily.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{bytes::decode_hex_array, limits::MAX_PATH_BYTES};

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,

    #[error("path is {actual} bytes; the maximum is {max}")]
    TooLong { actual: usize, max: u32 },

    #[error("path must be absolute: {path}")]
    NotAbsolute { path: String },

    #[error("path must be relative: {path}")]
    NotRelative { path: String },

    #[error("path contains a NUL byte")]
    NulByte,

    #[error("path has an empty, '.' or '..' segment or a trailing '/': {path}")]
    NotNormalized { path: String },

    #[error("entry name must be one segment other than '.' and '..': {name}")]
    NotAName { name: String },
}

/// JSON Schema pattern of [`EnvironmentPath`].
pub const ENVIRONMENT_PATH_PATTERN: &str = "^(/|(/(?!\\.\\.?(/|$))[^/\\u0000]+)+)$";
/// JSON Schema pattern of [`TreePath`].
pub const TREE_PATH_PATTERN: &str =
    "^(\\.|(?!\\.\\.?(/|$))[^/\\u0000]+(/(?!\\.\\.?(/|$))[^/\\u0000]+)*)$";
/// JSON Schema pattern of [`EntryName`].
pub const ENTRY_NAME_PATTERN: &str = "^(?!\\.\\.?$)[^/\\u0000]+$";
/// JSON Schema pattern of [`ContentDigest`].
pub const CONTENT_DIGEST_PATTERN: &str = "^sha256:[0-9a-f]{64}$";

/// An absolute, normalized POSIX path in the environment: `/`, or `/` and
/// one or more segments joined by `/`. Empty, `.` and `..` segments and a
/// trailing `/` are rejected, never rewritten. At most 4096 bytes.
/// `defaultCwd`, `homeDirectory`, every `cwd` and every path parameter use
/// this type; see the module's path model and confinement rule.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EnvironmentPath(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 4096), pattern(ENVIRONMENT_PATH_PATTERN))
    )]
    String,
);

impl EnvironmentPath {
    pub fn new(path: impl Into<String>) -> Result<Self, PathError> {
        let path = path.into();
        check_common(&path)?;
        if !path.starts_with('/') {
            return Err(PathError::NotAbsolute { path });
        }
        if path != "/" && !segments_normalized(&path[1..]) {
            return Err(PathError::NotNormalized { path });
        }
        Ok(Self(path))
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

    /// Path segments below the root.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|segment| !segment.is_empty())
    }

    pub fn file_name(&self) -> Option<&str> {
        self.segments().last()
    }

    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let index = self.0.rfind('/').unwrap_or(0);
        Some(if index == 0 {
            Self::root()
        } else {
            Self(self.0[..index].to_owned())
        })
    }

    /// Append a tree path. An empty tree path returns `self`.
    pub fn join(&self, tree: &TreePath) -> Result<Self, PathError> {
        if tree.is_root() {
            return Ok(self.clone());
        }
        let joined = if self.is_root() {
            format!("/{}", tree.as_str())
        } else {
            format!("{}/{}", self.0, tree.as_str())
        };
        Self::new(joined)
    }

    /// True when `self` is `base` or lies below it.
    pub fn starts_with(&self, base: &EnvironmentPath) -> bool {
        base.is_root()
            || self.0 == base.0
            || (self.0.starts_with(&base.0) && self.0.as_bytes().get(base.0.len()) == Some(&b'/'))
    }

    /// The tree path of `self` below `base`, or `None` when it is outside.
    pub fn relative_to(&self, base: &EnvironmentPath) -> Option<TreePath> {
        if !self.starts_with(base) {
            return None;
        }
        let rest = if base.is_root() {
            &self.0[1..]
        } else {
            self.0[base.0.len()..].trim_start_matches('/')
        };
        Some(if rest.is_empty() {
            TreePath::root()
        } else {
            TreePath(rest.to_owned())
        })
    }
}

/// A relative, normalized path below a selected root: segments joined by
/// `/`, without empty, `.` or `..` segments. The single segment `.` names
/// the root itself. At most 4096 bytes. Used for scan, inventory and
/// transfer entries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TreePath(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 4096), pattern(TREE_PATH_PATTERN))
    )]
    String,
);

impl Default for TreePath {
    fn default() -> Self {
        Self::root()
    }
}

impl TreePath {
    /// Wire spelling of the root.
    pub const ROOT: &'static str = ".";

    pub fn new(path: impl Into<String>) -> Result<Self, PathError> {
        let path = path.into();
        if path == Self::ROOT {
            return Ok(Self(path));
        }
        check_common(&path)?;
        if path.starts_with('/') {
            return Err(PathError::NotRelative { path });
        }
        if !segments_normalized(&path) {
            return Err(PathError::NotNormalized { path });
        }
        Ok(Self(path))
    }

    pub fn root() -> Self {
        Self(Self::ROOT.to_owned())
    }

    /// The wire spelling: `.` for the root.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == Self::ROOT
    }

    /// Segments below the root; none for the root itself.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        let path = if self.is_root() { "" } else { self.0.as_str() };
        path.split('/').filter(|segment| !segment.is_empty())
    }

    /// Number of segments: the root has depth 0, its direct children 1.
    pub fn depth(&self) -> u32 {
        self.segments().count() as u32
    }

    pub fn join(&self, segment: &str) -> Result<Self, PathError> {
        if self.is_root() {
            Self::new(segment)
        } else {
            Self::new(format!("{}/{segment}", self.0))
        }
    }
}

fn check_common(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.len() > MAX_PATH_BYTES as usize {
        return Err(PathError::TooLong {
            actual: path.len(),
            max: MAX_PATH_BYTES,
        });
    }
    if path.contains('\0') {
        return Err(PathError::NulByte);
    }
    Ok(())
}

fn segments_normalized(path: &str) -> bool {
    path.split('/')
        .all(|segment| !matches!(segment, "" | "." | ".."))
}

macro_rules! path_string_impls {
    ($name:ident) => {
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = PathError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = PathError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = PathError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

/// One entry name: a single path segment, neither `.` nor `..`, without
/// `/` or NUL, at most 4096 bytes. `fs/readDirectory` entries use it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryName(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 4096), pattern(ENTRY_NAME_PATTERN))
    )]
    String,
);

impl EntryName {
    pub fn new(name: impl Into<String>) -> Result<Self, PathError> {
        let name = name.into();
        check_common(&name)?;
        if name.contains('/') || name == "." || name == ".." {
            return Err(PathError::NotAName { name });
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

path_string_impls!(EnvironmentPath);
path_string_impls!(TreePath);
path_string_impls!(EntryName);

/// `sha256:` followed by 64 lowercase hex digits: the digest of a file's raw
/// bytes, independent of its path and mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContentDigest(
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", pattern(CONTENT_DIGEST_PATTERN))
    )]
    [u8; 32],
);

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("content digest must be sha256: followed by 64 lowercase hex digits")]
pub struct DigestError;

impl ContentDigest {
    pub const PREFIX: &'static str = "sha256:";

    pub fn from_sha256(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.0
    }

    /// The 64 lowercase hex digits without the prefix.
    pub fn hex(&self) -> String {
        crate::bytes::encode_hex(&self.0)
    }

    pub fn parse(value: &str) -> Result<Self, DigestError> {
        let hex = value.strip_prefix(Self::PREFIX).ok_or(DigestError)?;
        decode_hex_array::<32>(hex)
            .map(Self)
            .map_err(|_| DigestError)
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", Self::PREFIX, self.hex())
    }
}

impl FromStr for ContentDigest {
    type Err = DigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ContentDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_paths_are_rejected_rather_than_rewritten() {
        for good in ["/", "/workspace", "/a/b.c/d"] {
            assert_eq!(EnvironmentPath::new(good).unwrap().as_str(), good);
        }
        for bad in ["", "relative", "/../etc", "/a/./b", "/a//b", "/a/", "/a\0b"] {
            assert!(EnvironmentPath::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn backslashes_are_ordinary_name_bytes() {
        assert_eq!(
            EnvironmentPath::new("/a\\b").unwrap().file_name(),
            Some("a\\b")
        );
        assert!(EntryName::new("a\\b").is_ok());
        assert!(TreePath::new("dir/a\\b").is_ok());
    }

    #[test]
    fn tree_paths_are_relative_and_normalized() {
        let root = TreePath::new(".").unwrap();
        assert!(root.is_root());
        assert_eq!(root.depth(), 0);
        assert_eq!(serde_json::to_value(TreePath::root()).unwrap(), ".");
        assert_eq!(TreePath::root().join("a").unwrap().as_str(), "a");
        assert_eq!(TreePath::new("a/b").unwrap().depth(), 2);
        for bad in ["", "/a", "a/../b", "a//b", "./a", "a/", "a/."] {
            assert!(TreePath::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn entry_names_are_one_segment() {
        assert!(EntryName::new("notes.md").is_ok());
        for bad in ["", ".", "..", "a/b"] {
            assert!(EntryName::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn paths_join_and_relativize() {
        let base = EnvironmentPath::new("/workspace").unwrap();
        let tree = TreePath::new("src/lib.rs").unwrap();
        let joined = base.join(&tree).unwrap();
        assert_eq!(joined.as_str(), "/workspace/src/lib.rs");
        assert_eq!(joined.relative_to(&base), Some(tree));
        assert_eq!(base.relative_to(&base), Some(TreePath::root()));
        assert_eq!(base.join(&TreePath::root()).unwrap(), base);
        assert!(
            EnvironmentPath::new("/workspace2")
                .unwrap()
                .relative_to(&base)
                .is_none()
        );
        assert_eq!(joined.parent().unwrap().as_str(), "/workspace/src");
        assert_eq!(base.parent().unwrap(), EnvironmentPath::root());
    }

    #[test]
    fn digests_are_canonical_lowercase() {
        let text = format!("sha256:{}", "ab".repeat(32));
        let digest: ContentDigest = serde_json::from_value(serde_json::json!(text)).unwrap();
        assert_eq!(digest.to_string(), text);
        assert!(ContentDigest::parse(&format!("sha256:{}", "AB".repeat(32))).is_err());
        assert!(ContentDigest::parse(&"ab".repeat(32)).is_err());
    }
}
