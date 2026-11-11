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
//! **Names.** Paths are UTF-8. POSIX paths retain their native spelling;
//! Windows drive paths use an uppercase drive and `/` separators (`C:/src`).
//! Parsing never depends on the controller's OS. Windows names exclude device
//! names, alternate streams, control characters and trailing dots/spaces.
//! UNC, device namespaces and drive-relative paths are not supported.
//! An entry whose name cannot be represented is skipped and counted, never
//! returned lossily. Relative tree names retain POSIX grammar; a destination
//! must validate them against its own path convention before any side effect.

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

    #[error("unsupported Windows path: {path}")]
    UnsupportedWindowsPath { path: String },

    #[error("unsupported Windows entry name: {name}")]
    InvalidWindowsName { name: String },

    #[error("path does not use the requested convention: {path}")]
    WrongConvention { path: String },
}

/// Grammar of an environment's native paths, independent of the client's OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum PathConvention {
    /// Absolute `/`-separated paths; backslashes are ordinary name characters.
    Posix,
    /// Drive-absolute Windows paths. Wire separators are `/`; native ones `\`.
    Windows,
}

/// JSON Schema pattern of [`EnvironmentPath`].
pub const ENVIRONMENT_PATH_PATTERN: &str = concat!(
    "^(/|(/(?!\\.\\.?(/|$))[^/\\u0000]+)+|[A-Z]:/(",
    "(?!(?:[cC][oO][nN]|[pP][rR][nN]|[aA][uU][xX]|[nN][uU][lL]|",
    "[cC][oO][mM][1-9¹²³]|[lL][pP][tT][1-9¹²³]|[cC][oO][nN][iI][nN]\\$|[cC][oO][nN][oO][uU][tT]\\$) *(?:\\.|/|$))",
    "[^/\\\\<>:\"|?*\\u0000-\\u001f]*[^/\\\\<>:\"|?*\\u0000-\\u001f .]",
    "(/(?!(?:[cC][oO][nN]|[pP][rR][nN]|[aA][uU][xX]|[nN][uU][lL]|",
    "[cC][oO][mM][1-9¹²³]|[lL][pP][tT][1-9¹²³]|[cC][oO][nN][iI][nN]\\$|[cC][oO][nN][oO][uU][tT]\\$) *(?:\\.|/|$))",
    "[^/\\\\<>:\"|?*\\u0000-\\u001f]*[^/\\\\<>:\"|?*\\u0000-\\u001f .])*",
    ")?)(?![\\s\\S])"
);
/// JSON Schema pattern of [`TreePath`].
pub const TREE_PATH_PATTERN: &str =
    "^(\\.|(?!\\.\\.?(/|$))[^/\\u0000]+(/(?!\\.\\.?(/|$))[^/\\u0000]+)*)$";
/// JSON Schema pattern of [`EntryName`].
pub const ENTRY_NAME_PATTERN: &str = "^(?!\\.\\.?$)[^/\\u0000]+$";
/// JSON Schema pattern of [`ContentDigest`].
pub const CONTENT_DIGEST_PATTERN: &str = "^sha256:[0-9a-f]{64}$";

/// An absolute, normalized environment path: `/` or `C:/`, optionally followed
/// by segments joined by `/`. Empty, `.` and `..` segments and a trailing `/`
/// below the root are rejected, never rewritten. At most 4096 UTF-8 bytes.
/// Equality and ordering are exact lexical comparisons, not file identity or
/// authorization. Windows confinement must use native handles and identity;
/// neither case folding nor these lexical helpers establish confinement.
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
        let root_len = if path.starts_with('/') {
            1
        } else if path.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
            && path.as_bytes().get(1..3) == Some(b":/")
        {
            3
        } else {
            return Err(PathError::NotAbsolute { path });
        };
        if path.len() != root_len && !segments_normalized(&path[root_len..]) {
            return Err(PathError::NotNormalized { path });
        }
        if root_len == 3 && path.len() != root_len {
            for name in path[root_len..].split('/') {
                validate_windows_name(name)?;
            }
        }
        Ok(Self(path))
    }

    /// Convert native spelling using the explicitly selected remote convention.
    /// Only drive case and Windows separators are normalized. No URI decoding,
    /// dot-segment resolution, extended-prefix stripping or case folding occurs.
    pub fn from_native(path: &str, convention: PathConvention) -> Result<Self, PathError> {
        let value = match convention {
            PathConvention::Posix => Self::new(path)?,
            PathConvention::Windows => {
                check_common(path)?;
                if !path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                    || path.as_bytes().get(1) != Some(&b':')
                    || !matches!(path.as_bytes().get(2), Some(b'/' | b'\\'))
                {
                    return Err(PathError::UnsupportedWindowsPath { path: path.into() });
                }
                let mut canonical = path.replace('\\', "/");
                canonical[..1].make_ascii_uppercase();
                Self::new(canonical)?
            }
        };
        if value.convention() != convention {
            return Err(PathError::WrongConvention { path: path.into() });
        }
        Ok(value)
    }

    pub fn convention(&self) -> PathConvention {
        if self.0.starts_with('/') {
            PathConvention::Posix
        } else {
            PathConvention::Windows
        }
    }

    /// Native display spelling for the remote host, not a local `PathBuf`.
    pub fn to_native(&self) -> String {
        match self.convention() {
            PathConvention::Posix => self.0.clone(),
            PathConvention::Windows => self.0.replace('/', "\\"),
        }
    }

    fn root_len(&self) -> usize {
        match self.convention() {
            PathConvention::Posix => 1,
            PathConvention::Windows => 3,
        }
    }

    pub fn root() -> Self {
        Self("/".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.len() == self.root_len()
    }

    /// Path segments below the root.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0[self.root_len()..]
            .split('/')
            .filter(|segment| !segment.is_empty())
    }

    pub fn file_name(&self) -> Option<&str> {
        self.segments().last()
    }

    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let index = self.0.rfind('/').unwrap_or(0);
        Some(if index < self.root_len() {
            Self(self.0[..self.root_len()].to_owned())
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
            format!("{}{}", self.0, tree.as_str())
        } else {
            format!("{}/{}", self.0, tree.as_str())
        };
        Self::new(joined)
    }

    /// Lexically below `base`. This is not a filesystem confinement check.
    pub fn starts_with(&self, base: &EnvironmentPath) -> bool {
        (base.is_root() && self.0.starts_with(&base.0))
            || self.0 == base.0
            || (self.0.starts_with(&base.0) && self.0.as_bytes().get(base.0.len()) == Some(&b'/'))
    }

    /// The tree path of `self` below `base`, or `None` when it is outside.
    pub fn relative_to(&self, base: &EnvironmentPath) -> Option<TreePath> {
        if !self.starts_with(base) {
            return None;
        }
        let rest = if base.is_root() {
            &self.0[base.root_len()..]
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

fn validate_windows_name(name: &str) -> Result<(), PathError> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if reserved
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c <= '\u{1f}' || "\\<>:\"|?*".contains(c))
    {
        return Err(PathError::InvalidWindowsName { name: name.into() });
    }
    Ok(())
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
    fn windows_paths_parse_and_convert_without_host_path_rules() {
        let root = EnvironmentPath::new("C:/").unwrap();
        assert!(root.is_root());
        assert_eq!(root.parent(), None);
        assert_eq!(root.file_name(), None);
        assert_eq!(root.segments().count(), 0);
        let path =
            EnvironmentPath::from_native(r"c:\work space\雪.txt", PathConvention::Windows).unwrap();
        assert_eq!(path.as_str(), "C:/work space/雪.txt");
        assert_eq!(path.to_native(), "C:\\work space\\雪.txt");
        assert_eq!(
            path.segments().collect::<Vec<_>>(),
            ["work space", "雪.txt"]
        );
        let relative = TreePath::new("work space/雪.txt").unwrap();
        assert_eq!(root.join(&relative).unwrap(), path);
        assert_eq!(path.relative_to(&root), Some(relative));
        assert_eq!(path.parent().unwrap().parent(), Some(root.clone()));
        assert!(!path.starts_with(&EnvironmentPath::root()));
        assert!(!path.starts_with(&EnvironmentPath::new("D:/").unwrap()));
        assert!(!path.starts_with(&EnvironmentPath::new("C:/work").unwrap()));
        assert!(!EnvironmentPath::root().starts_with(&root));
        assert_ne!(
            EnvironmentPath::new("C:/WORK").unwrap(),
            EnvironmentPath::new("C:/work").unwrap()
        );
        assert!(matches!(
            EnvironmentPath::from_native("C:/x", PathConvention::Posix),
            Err(PathError::WrongConvention { .. })
        ));
        assert_eq!(
            serde_json::from_str::<EnvironmentPath>(r#""C:/work space/雪.txt""#).unwrap(),
            path
        );
    }

    #[test]
    fn windows_names_and_unsupported_namespaces_are_refused() {
        for path in [
            r"\\server\share\x",
            r"\\?\C:\x",
            r"\\.\C:\x",
            "C:relative",
            r"\rooted",
            "/c/work",
            "file:///C:/work",
        ] {
            assert!(
                matches!(
                    EnvironmentPath::from_native(path, PathConvention::Windows),
                    Err(PathError::UnsupportedWindowsPath { .. })
                ),
                "{path}"
            );
        }
        let root = EnvironmentPath::new("C:/").unwrap();
        for name in [
            "CON",
            "nul.txt",
            "COM1",
            "LPT².log",
            "CONIN$",
            "CONOUT$",
            "con .txt",
            "a.",
            "a ",
            "a:b",
            "a?b",
            "a*b",
            "a<b",
            "a>b",
            "a|b",
            "a\"b",
            "a\\b",
            "a\u{1f}b",
        ] {
            assert!(
                matches!(
                    root.join(&TreePath::new(name).unwrap()),
                    Err(PathError::InvalidWindowsName { .. })
                ),
                "{name:?}"
            );
            // The same names retain their existing POSIX meaning.
            assert!(
                EnvironmentPath::root()
                    .join(&TreePath::new(name).unwrap())
                    .is_ok()
            );
        }
        for path in [
            "c:/x",
            "C:/a/../b",
            "C:/a//b",
            "C:/a/",
            "C:/.",
            "C:/..",
            "C:\\x",
        ] {
            assert!(EnvironmentPath::new(path).is_err(), "{path}");
        }
        for path in [
            "C:/COM10",
            "C:/CON notes.txt",
            "C:/console",
            "C:/file%20name",
            "C:/a#b",
            "C:/雪",
            "C:/.hidden",
        ] {
            assert_eq!(EnvironmentPath::new(path).unwrap().as_str(), path);
        }
    }

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
