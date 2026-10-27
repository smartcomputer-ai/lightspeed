//! `fs/*`: path-based filesystem methods.
//!
//! Every path is an absolute [`EnvironmentPath`] resolved under the
//! confinement rule stated on that type. Methods that change the
//! filesystem need `capabilities.filesystem.write`; without it they answer
//! `capabilityUnavailable` (`filesystemWrite`).
//!
//! **Pattern grammar** ([`TreeFilter`]), shared by `fs/searchText`,
//! `fs/globFiles`, `fs/scan` and transfer captures. Patterns match paths
//! relative to the walk's root, gitignore-style: `*` and `?` match within
//! one segment, `**` spans segments, `[...]` is a character class and
//! `{a,b}` an alternation. A pattern without `/` matches the entry's name
//! at any depth; a leading `/` anchors it to the root. Walks always prune
//! VCS metadata directories (`.git`, `.hg`, `.svn`, `.jj`) and the daemon
//! state directory; by default they also respect ignore files (`.gitignore`,
//! `.ignore`, git excludes) and skip hidden entries. An entry a walk cannot
//! represent or read (a name that is not UTF-8, a directory that cannot be
//! listed) is skipped and counted in the result's `skippedEntryCount`;
//! it never ends the walk.
//!
//! **Two entry shapes, on purpose.** [`FileMetadata`] describes one path as
//! it is now (`fs/getMetadata`, `fs/readDirectory`): kind including
//! symlinks, times and a `version` for preconditions. [`TreeContent`]
//! describes what a tree holds (`fs/scan` entries, transfer inventories):
//! directories and files with size, mode and digest, the facts two trees
//! are compared by. Trees never hold symlink entries: scans skip or follow
//! links and captures expand or skip them.

use serde::{Deserialize, Serialize};

use crate::{
    bytes::ByteChunk,
    ids::{DirectoryCursor, validate_opaque_id},
    paths::{ContentDigest, EntryName, EnvironmentPath},
    serde_util::{is_false, is_zero_u32},
};

/// Kind of a filesystem entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    /// Device, FIFO, socket or another special file.
    Other,
}

/// One file-entry shape for `fs/getMetadata` and `fs/readDirectory`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileMetadata {
    pub kind: FileKind,
    /// Size in bytes; for a symlink, the length of its target text.
    pub size_bytes: u64,
    /// A regular file with any execute bit set; always false for
    /// directories, symlinks and special files.
    #[serde(default, skip_serializing_if = "is_false")]
    pub executable: bool,
    pub modified_at_ms: i64,
    /// Absent when the filesystem records no birth time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<i64>,
    /// Target text of a symlink, when it is UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    /// Changes whenever the entry's content or identity changes; compare it
    /// with `ifMatch`.
    pub version: FileVersion,
}

/// Opaque version token of a file's content and identity. Equal tokens for
/// one path mean the content is unchanged. 1 to 255 bytes without control
/// characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileVersion(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 255), pattern(crate::ids::NO_CONTROL_PATTERN))
    )]
    String,
);

impl FileVersion {
    pub fn new(value: impl Into<String>) -> Result<Self, crate::ids::IdError> {
        let value = value.into();
        validate_opaque_id("FileVersion", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for FileVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FileVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// What a tree entry is: a scan entry or a transfer inventory entry.
/// Digests identify raw file bytes, not paths or modes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TreeContent {
    Directory {},
    File {
        size_bytes: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        executable: bool,
        /// Scans: present when `includeDigests` was set. Transfer
        /// inventories: always present; an appended file entry without it
        /// is `invalidParams`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<ContentDigest>,
    },
}
/// `fs/readFile` params. Reads a regular file only; anything else is
/// `failedPrecondition` (`notRegularFile`) and is never opened for a
/// blocking read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileParams {
    pub path: EnvironmentPath,
    /// First byte to return; 0 by default.
    #[serde(default, skip_serializing_if = "crate::serde_util::is_zero_u64")]
    pub offset: u64,
    /// Most bytes to return, at most `limits.filesystem.maxReadFileBytes`.
    pub max_bytes: u64,
}

/// `fs/readFile` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileResponse {
    pub data: ByteChunk,
    /// Size of the whole file when it was read.
    pub file_size: u64,
    /// Bytes exist after `offset` plus the length of `data`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    pub version: FileVersion,
}

/// When `fs/writeFile` may write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum WriteCondition {
    /// Create or replace.
    #[default]
    Always,
    /// Only create; an existing entry is `failedPrecondition`
    /// (`createCondition`).
    IfAbsent,
    /// Only replace; a missing file is `failedPrecondition`
    /// (`createCondition`).
    IfPresent,
}

impl WriteCondition {
    pub fn is_default(&self) -> bool {
        *self == Self::Always
    }
}

/// `fs/writeFile` params. The write is staged in the target's directory and
/// published by an atomic rename: readers see the old or the new content,
/// never a mix. An existing target keeps its permissions. A symlink at
/// `path` whose target stays inside the root is followed like any path: the
/// final target is replaced and the link itself is kept (a dangling link
/// creates its target, subject to `create`); a link that leaves the root is
/// `permissionDenied` (`outsideRoot`). A special file is
/// `failedPrecondition` (`notRegularFile`), a directory
/// `failedPrecondition` (`isDirectory`). The parent of the final target
/// must exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteFileParams {
    pub path: EnvironmentPath,
    /// At most `limits.filesystem.maxWriteFileBytes`; larger files go
    /// through `transfer/*` with a `writeFile` selection.
    pub data: ByteChunk,
    #[serde(default, skip_serializing_if = "WriteCondition::is_default")]
    pub create: WriteCondition,
    /// Write only when the current file's version equals this token;
    /// otherwise `failedPrecondition` (`ifMatchFailed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_match: Option<FileVersion>,
    /// Set the execute bits (the read bits' pattern). Without it a new file
    /// gets the default mode and an existing file keeps its mode.
    #[serde(default, skip_serializing_if = "is_false")]
    pub executable: bool,
}

/// `fs/writeFile` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteFileResponse {
    /// Version of the written file.
    pub version: FileVersion,
}

/// `fs/createDirectory` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDirectoryParams {
    pub path: EnvironmentPath,
    /// Create missing parents, and succeed when the directory exists.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recursive: bool,
}

/// `fs/createDirectory` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDirectoryResponse {}

/// `fs/getMetadata` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetMetadataParams {
    pub path: EnvironmentPath,
    /// Describe the symlink's target instead of the link (subject to
    /// confinement).
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_symlinks: bool,
}

/// `fs/getMetadata` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetMetadataResponse {
    pub metadata: FileMetadata,
}

/// `fs/readDirectory` params. Pages in name order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadDirectoryParams {
    /// A directory, or a symlink to one inside the root.
    pub path: EnvironmentPath,
    /// Most entries per page, 1 to `limits.filesystem.maxDirectoryEntries`.
    pub limit: u32,
    /// `nextCursor` of the previous page; a cursor this daemon did not mint
    /// is `invalidParams`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<DirectoryCursor>,
}

/// `fs/readDirectory` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadDirectoryResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<DirectoryEntry>,
    /// Opaque cursor of the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<DirectoryCursor>,
    /// Entries in this page's range skipped because their names are not
    /// UTF-8. Entries that vanish while the page is built are left out and
    /// not counted.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub skipped_entry_count: u32,
}

/// One `fs/readDirectory` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryEntry {
    pub name: EntryName,
    /// The entry itself, not a symlink's target. Entries that vanish while
    /// the page is built are left out.
    pub metadata: FileMetadata,
}

/// `fs/remove` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveParams {
    /// A symlink is removed, never followed.
    pub path: EnvironmentPath,
    /// Remove a directory and its contents.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recursive: bool,
    /// Succeed when the path does not exist.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

/// `fs/remove` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveResponse {}

/// `fs/rename` params. Atomic within one filesystem; across filesystems it
/// is `failedPrecondition` (`crossDevice`). The entry keeps its mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameParams {
    pub source: EnvironmentPath,
    pub destination: EnvironmentPath,
    /// Replace an existing destination file; otherwise an existing
    /// destination is `alreadyExists`. A directory destination is never
    /// replaced.
    #[serde(default, skip_serializing_if = "is_false")]
    pub overwrite: bool,
    /// Rename only when the source's version equals this token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_match: Option<FileVersion>,
}

/// `fs/rename` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameResponse {}

/// Which entries a walk visits. The default visits everything except
/// hidden and ignored entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TreeFilter {
    /// Select entries matching any pattern; empty selects every entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Drop entries matching any pattern; a matching directory is pruned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Match patterns case-insensitively.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignore_case: bool,
    /// Visit entries whose name starts with `.`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_hidden: bool,
    /// Visit entries that ignore files exclude.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_ignored: bool,
    /// Deepest entry depth visited: the root's direct children are depth
    /// 1, so `1` selects direct children only. Unbounded when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
}

impl TreeFilter {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `fs/searchText` params: a bounded recursive regex search run by the
/// daemon. A file larger than the remaining byte budget is skipped and
/// counted; it never ends the walk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchTextParams {
    pub root: EnvironmentPath,
    /// Regular expression in the Rust `regex` crate's syntax, matched per
    /// line.
    pub pattern: String,
    /// Match `pattern` case-insensitively. `filter.ignoreCase` governs the
    /// path patterns instead.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pattern_ignore_case: bool,
    #[serde(default, skip_serializing_if = "TreeFilter::is_default")]
    pub filter: TreeFilter,
    pub limits: SearchTextLimits,
}

/// Caller quotas of one search; each at most its advertised ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchTextLimits {
    pub max_matches: u32,
    /// Files searched, not merely visited.
    pub max_files: u32,
    /// Bytes actually read across files.
    pub max_bytes: u64,
    pub max_duration_ms: u64,
    /// Longer matching lines are cut and marked `lineTruncated`.
    pub max_line_bytes: u32,
}

/// `fs/searchText` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchTextResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<SearchTextMatch>,
    pub searched_file_count: u32,
    pub searched_bytes: u64,
    /// Files skipped because they exceeded the remaining byte budget, are
    /// binary or could not be read.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub skipped_file_count: u32,
    /// Entries the walk could not represent or list: names that are not
    /// UTF-8 and directories that could not be read.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub skipped_entry_count: u32,
    pub elapsed_ms: u64,
    /// Absent when the walk finished within its limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<SearchTextStop>,
}

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchTextMatch {
    pub path: EnvironmentPath,
    /// 1-based.
    pub line_number: u32,
    /// The matching line without its terminator, cut at `maxLineBytes` on a
    /// character boundary.
    pub line: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub line_truncated: bool,
}

/// Why a search stopped before finishing its walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SearchTextStop {
    /// `maxMatches` matches were found.
    MatchLimit,
    /// `maxFiles` files were searched.
    FileLimit,
    /// `maxBytes` bytes were read.
    ByteLimit,
    /// `maxDurationMs` ran out.
    TimeLimit,
    /// The next match would push the encoded matches past
    /// [`MAX_RESULT_BYTES`](crate::limits::MAX_RESULT_BYTES).
    ResponseLimit,
}

/// `fs/globFiles` params: bounded recursive enumeration of non-directory
/// entries matching `filter.include`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobFilesParams {
    pub root: EnvironmentPath,
    /// `include` must not be empty.
    pub filter: TreeFilter,
    pub limits: GlobFilesLimits,
}

/// Caller quotas of one enumeration; each at most its advertised ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobFilesLimits {
    pub max_matches: u32,
    /// Non-directory entries visited.
    pub max_files: u32,
    pub max_duration_ms: u64,
}

/// `fs/globFiles` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobFilesResponse {
    /// In walk order (sorted by path).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<EnvironmentPath>,
    pub visited_file_count: u32,
    /// Entries the walk could not represent or list: names that are not
    /// UTF-8 and directories that could not be read.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub skipped_entry_count: u32,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<GlobFilesStop>,
}

/// Why an enumeration stopped before finishing its walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum GlobFilesStop {
    /// `maxMatches` paths were found.
    MatchLimit,
    /// `maxFiles` non-directory entries were visited.
    FileLimit,
    /// `maxDurationMs` ran out.
    TimeLimit,
    /// The next match would push the encoded matches past
    /// [`MAX_RESULT_BYTES`](crate::limits::MAX_RESULT_BYTES).
    ResponseLimit,
}
