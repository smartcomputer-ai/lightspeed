//! The one confinement rule of every data-plane path, a command's `cwd`
//! included.
//!
//! A wire path that is neither the filesystem root nor below it is
//! `permissionDenied` (`outsideRoot`). Otherwise the root prefix is stripped
//! and the remaining segments are resolved one at a time beneath the root's
//! directory descriptor. A symlink is followed only while its target stays
//! inside the root; a link that leaves it is refused the same way. A path
//! that names the daemon state directory, or passes through or below it, is
//! `permissionDenied` (`daemonState`). So is a path that reaches a kernel
//! filesystem or the registration key (see [`Protected`]): to a caller they
//! are the daemon's own, like its state.
//!
//! The state directory is recognized by its identity (device and inode),
//! never by name: every entry a lookup meets is compared with it, so another
//! spelling on a case-insensitive filesystem, a bind mount or a link cannot
//! reach it. The protected locations are recognized the same way, by device
//! or by the identity of the file a path names now.

use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use environment_protocol::{
    error::{PermissionReason, PreconditionReason, ProtocolError},
    paths::EnvironmentPath,
};

use super::{
    io_error,
    os::{DEFAULT_DIRECTORY_MODE, Directory, EntryStat, SELF},
    protected::{Protected, ProtectedFiles},
};

/// Most symlinks one resolution follows before it is a loop.
const MAX_SYMLINK_FOLLOWS: u32 = 40;

/// The filesystem root, its directory descriptor, the daemon state
/// directory and the other locations file operations never reach. Built
/// once per daemon and shared by every manager.
#[derive(Debug)]
pub struct Confinement {
    root: PathBuf,
    root_path: EnvironmentPath,
    root_directory: Directory,
    state_dir: PathBuf,
    /// Device and inode of the state directory.
    state_identity: Identity,
    /// Device and inode of each of its ancestors, its parent first.
    state_ancestors: Vec<Identity>,
    /// Kernel filesystems and the registration key.
    protected: Protected,
    /// Keep native state identity pinned against rename/replacement while served.
    #[cfg(windows)]
    _state_directory: Directory,
}

/// Device and inode of an entry.
pub(crate) type Identity = (u64, u128);

/// Keep the resolved directory and its ancestry alive until native spawning has
/// consumed the cwd. The path alone is not a confinement token on Windows.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct PinnedDirectory {
    path: PathBuf,
    _directory: Directory,
}

#[cfg(windows)]
impl PinnedDirectory {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// How one lookup treats the final entry and missing entries.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Lookup {
    /// Follow a symlink in the final position (inside the root only).
    pub follow_final: bool,
    /// A missing final entry resolves with `stat: None` instead of
    /// `notFound`.
    pub allow_missing: bool,
    /// Create missing intermediate directories.
    pub create_parents: bool,
    /// Refuse every symlink, final or not: re-opening a path recorded as
    /// canonical, where a link means the tree changed.
    pub no_symlinks: bool,
}

impl Lookup {
    pub const FOLLOW: Self = Self {
        follow_final: true,
        allow_missing: false,
        create_parents: false,
        no_symlinks: false,
    };
    pub const NO_FOLLOW: Self = Self {
        follow_final: false,
        allow_missing: false,
        create_parents: false,
        no_symlinks: false,
    };

    pub fn allowing_missing(mut self) -> Self {
        self.allow_missing = true;
        self
    }

    pub fn creating_parents(mut self) -> Self {
        self.create_parents = true;
        self
    }
}

/// A resolved entry: the directory that holds it and its name there. Every
/// later operation names the entry through `parent` and `name`, so a link
/// swapped in after resolution is never followed.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub parent: Directory,
    /// `.` for the filesystem root itself.
    pub name: OsString,
    /// Canonical segments below the root; empty for the root.
    pub segments: Vec<OsString>,
    /// `lstat` of the final entry; `None` when it is missing and the lookup
    /// allowed that.
    pub stat: Option<EntryStat>,
    /// Canonical segments of the directories this lookup created
    /// (`creating_parents`), outermost first.
    #[cfg_attr(windows, allow(dead_code))] // Used by transfer publication on Unix.
    pub created: Vec<Vec<OsString>>,
}

impl Resolved {
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// Open the resolved directory.
    pub fn open_directory(&self) -> std::io::Result<Directory> {
        if self.is_root() {
            self.parent.try_clone()
        } else {
            self.parent.child(&self.name)
        }
    }
}

impl Confinement {
    #[cfg(windows)]
    pub(crate) fn root_directory(&self) -> Directory {
        self.root_directory.clone()
    }
    /// `root` is canonicalized and opened. `state_dir` is created (private)
    /// when missing and canonicalized; it must not be the root itself.
    /// Nothing else is refused: a serving daemon uses [`Self::protecting`].
    pub fn new(root: PathBuf, state_dir: PathBuf) -> Result<Self, ProtocolError> {
        Self::protecting(root, state_dir, Protected::default())
    }

    /// [`Self::new`], also refusing every location in `protected`.
    pub fn protecting(
        root: PathBuf,
        state_dir: PathBuf,
        protected: Protected,
    ) -> Result<Self, ProtocolError> {
        let root = root
            .canonicalize()
            .map_err(|error| io_error(error, root.display()))?;
        #[cfg(windows)]
        let root = crate::windows_security::local_drive_path(&root)
            .map_err(|error| io_error(error, root.display()))?;
        let root_path = root
            .to_str()
            .and_then(|root| {
                EnvironmentPath::from_native(
                    root,
                    if cfg!(windows) {
                        environment_protocol::paths::PathConvention::Windows
                    } else {
                        environment_protocol::paths::PathConvention::Posix
                    },
                )
                .ok()
            })
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "filesystem root {} is not a valid environment path",
                    root.display()
                ))
            })?;
        let root_directory =
            Directory::open_host(&root).map_err(|error| io_error(error, root.display()))?;
        #[cfg(unix)]
        if !state_dir.exists() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(super::os::PRIVATE_DIRECTORY_MODE)
                .create(&state_dir)
                .map_err(|error| io_error(error, state_dir.display()))?;
        }
        #[cfg(windows)]
        crate::windows_security::prepare_private_directory(&state_dir)
            .map_err(|error| io_error(error, state_dir.display()))?;
        let state_dir = state_dir
            .canonicalize()
            .map_err(|error| io_error(error, state_dir.display()))?;
        #[cfg(windows)]
        let state_dir = crate::windows_security::local_drive_path(&state_dir)
            .map_err(|error| io_error(error, state_dir.display()))?;
        let identity_of = |path: &Path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(path)
                    .map(|metadata| (metadata.dev(), metadata.ino() as u128))
                    .map_err(|error| io_error(error, path.display()))
            }
            #[cfg(windows)]
            Directory::open_host(path)
                .and_then(|directory| EntryStat::of_directory(&directory))
                .map(|stat| stat.identity())
                .map_err(|error| io_error(error, path.display()))
        };
        #[cfg(unix)]
        let state_identity = identity_of(&state_dir)?;
        #[cfg(windows)]
        let state_directory = Directory::open_host(&state_dir)
            .map_err(|error| io_error(error, state_dir.display()))?;
        #[cfg(windows)]
        let state_identity = EntryStat::of_directory(&state_directory)
            .map_err(|error| io_error(error, state_dir.display()))?
            .identity();
        let state_ancestors = state_dir
            .ancestors()
            .skip(1)
            .map(identity_of)
            .collect::<Result<Vec<_>, _>>()?;
        let root_identity = EntryStat::of_directory(&root_directory)
            .map_err(|error| io_error(error, root.display()))?
            .identity();
        if root_identity == state_identity {
            return Err(ProtocolError::internal(
                "the daemon state directory must not be the filesystem root",
            ));
        }
        Ok(Self {
            root,
            root_path,
            root_directory,
            state_dir,
            state_identity,
            state_ancestors,
            protected,
            #[cfg(windows)]
            _state_directory: state_directory,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The root as a wire path.
    pub fn root_path(&self) -> &EnvironmentPath {
        &self.root_path
    }

    /// The confinement rule: the resolved host path of the final target,
    /// following in-root symlinks. Missing entries are `notFound`.
    pub fn resolve(&self, path: &EnvironmentPath) -> Result<PathBuf, ProtocolError> {
        let resolved = self.lookup(path, Lookup::FOLLOW)?;
        Ok(self.host_path(&resolved.segments))
    }

    /// [`Self::resolve`], then require a directory
    /// (`failedPrecondition` `notDirectory` otherwise).
    pub fn resolve_directory(&self, path: &EnvironmentPath) -> Result<PathBuf, ProtocolError> {
        let resolved = self.lookup(path, Lookup::FOLLOW)?;
        if !resolved.stat.is_some_and(|stat| stat.is_dir()) {
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::NotDirectory,
                format!("{path} is not a directory"),
            ));
        }
        Ok(self.host_path(&resolved.segments))
    }

    #[cfg(windows)]
    pub(crate) fn pin_directory(
        &self,
        path: &EnvironmentPath,
    ) -> Result<PinnedDirectory, ProtocolError> {
        let resolved = self.lookup(path, Lookup::FOLLOW)?;
        let directory = resolved
            .open_directory()
            .map_err(|error| io_error(error, path))?;
        let stat = EntryStat::of_directory(&directory).map_err(|error| io_error(error, path))?;
        self.admit(&self.protected_files(), &stat, path)?;
        // CreateProcess consumes an ordinary DOS path, whereas the native
        // resolver can encounter literal names through reparse targets. Reject
        // names DOS would normalize (such as trailing dots) before spawning.
        let canonical = self.wire_path(&resolved.segments).ok_or_else(|| {
            ProtocolError::invalid_field(
                "cwd",
                "resolved cwd cannot be represented without Windows path normalization",
            )
        })?;
        Ok(PinnedDirectory {
            path: PathBuf::from(canonical.to_native()),
            _directory: directory,
        })
    }

    /// Resolve a wire path under the rule.
    pub(crate) fn lookup(
        &self,
        path: &EnvironmentPath,
        lookup: Lookup,
    ) -> Result<Resolved, ProtocolError> {
        let Some(relative) = path.relative_to(&self.root_path) else {
            return Err(outside_root(path));
        };
        let components = relative.segments().map(OsString::from).collect();
        self.walk(components, lookup, path)
    }

    /// Resolve canonical segments below the root plus a trailing name,
    /// following a symlink in final position: how a walker follows a link
    /// it met at `segments/name`.
    pub(crate) fn lookup_link(
        &self,
        segments: &[OsString],
        name: &OsStr,
        subject: &dyn std::fmt::Display,
    ) -> Result<Resolved, ProtocolError> {
        let mut components = segments.to_vec();
        components.push(name.to_owned());
        self.walk(components.into(), Lookup::FOLLOW, subject)
    }

    /// Re-open canonical segments recorded earlier. Any symlink on the way
    /// means the tree changed: it fails with `ELOOP`'s mapping.
    pub(crate) fn reopen(
        &self,
        segments: &[OsString],
        subject: &dyn std::fmt::Display,
    ) -> Result<Resolved, ProtocolError> {
        let lookup = Lookup {
            no_symlinks: true,
            ..Lookup::NO_FOLLOW
        };
        self.walk(segments.iter().cloned().collect(), lookup, subject)
    }

    /// The host path of canonical segments.
    pub(crate) fn host_path(&self, segments: &[OsString]) -> PathBuf {
        let mut path = self.root.clone();
        path.extend(segments);
        path
    }

    /// The wire path of canonical segments, when they are UTF-8.
    pub(crate) fn wire_path(&self, segments: &[OsString]) -> Option<EnvironmentPath> {
        if segments.is_empty() {
            return Some(self.root_path.clone());
        }
        let mut path = self.root_path.as_str().trim_end_matches('/').to_owned();
        for segment in segments {
            path.push('/');
            path.push_str(segment.to_str()?);
        }
        if path.is_empty() {
            path.push('/');
        }
        EnvironmentPath::new(path).ok()
    }

    /// The entry is the state directory.
    pub(crate) fn is_state(&self, stat: &EntryStat) -> bool {
        stat.identity() == self.state_identity
    }

    /// The entry is the state directory or one of its ancestors: removing,
    /// replacing or capturing it would address the state directory.
    pub(crate) fn contains_state(&self, stat: &EntryStat) -> bool {
        self.is_state(stat) || self.state_ancestors.contains(&stat.identity())
    }

    /// The directory is the state directory's parent, so one of its
    /// entries may be the state directory.
    pub(crate) fn holds_state(&self, directory: &EntryStat) -> bool {
        self.state_ancestors.first() == Some(&directory.identity())
    }

    /// The protected files as they are now. One lookup, walk or listing
    /// identifies them once, as it starts.
    pub(crate) fn protected_files(&self) -> ProtectedFiles {
        self.protected.files_now()
    }

    /// The entry is one no path addresses: the state directory, an entry of
    /// a refused filesystem, or one of `files`. Walks prune such an entry
    /// and listings leave it out.
    pub(crate) fn refuses(&self, files: &ProtectedFiles, stat: &EntryStat) -> bool {
        self.is_state(stat) || self.protected.covers(files, stat)
    }

    /// Refuse a lookup that met an entry no path addresses.
    fn admit(
        &self,
        files: &ProtectedFiles,
        stat: &EntryStat,
        subject: &dyn std::fmt::Display,
    ) -> Result<(), ProtocolError> {
        if self.is_state(stat) {
            return Err(daemon_state(subject));
        }
        if self.protected.covers(files, stat) {
            return Err(protected_location(subject));
        }
        Ok(())
    }

    /// Refuse an operation whose subject contains the state directory or a
    /// protected file: once the directory around one is moved, no rule
    /// would find it again.
    pub(crate) fn refuse_containing_state(
        &self,
        resolved: &Resolved,
        subject: &dyn std::fmt::Display,
    ) -> Result<(), ProtocolError> {
        let Some(stat) = resolved.stat else {
            return Ok(());
        };
        if self.contains_state(&stat) {
            return Err(daemon_state(subject));
        }
        if stat.is_dir() && self.protected.holds_file(&stat) {
            return Err(protected_location(subject));
        }
        Ok(())
    }

    /// Remove directories a lookup created (see [`Resolved::created`])
    /// that are still empty, innermost first. Best effort.
    pub(crate) fn remove_created(&self, created: &[Vec<OsString>]) {
        for segments in created.iter().rev() {
            let Some((name, parent)) = segments.split_last() else {
                continue;
            };
            let Ok(resolved) = self.reopen(parent, &"created directory") else {
                return;
            };
            let Ok(directory) = resolved.open_directory() else {
                return;
            };
            if directory.rmdir(name).is_err() {
                return;
            }
        }
    }

    fn walk(
        &self,
        mut pending: VecDeque<OsString>,
        lookup: Lookup,
        subject: &dyn std::fmt::Display,
    ) -> Result<Resolved, ProtocolError> {
        let io = |error| io_error(error, subject);
        let files = self.protected_files();
        let mut stack = vec![self.root_directory.try_clone().map_err(io)?];
        let mut segments: Vec<OsString> = Vec::new();
        let mut created = Vec::new();
        let mut follows = 0;
        while let Some(name) = pending.pop_front() {
            if name.is_empty() || name == SELF {
                continue;
            }
            if name == ".." {
                if segments.pop().is_none() {
                    return Err(outside_root(subject));
                }
                stack.pop();
                continue;
            }
            let last = pending.is_empty();
            let mut candidate = segments.clone();
            candidate.push(name.clone());
            let directory = stack.last().expect("the root stays on the stack");
            let stat = match directory.stat(&name) {
                Ok(stat) => stat,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if last && lookup.allow_missing {
                        return Ok(Resolved {
                            parent: directory.try_clone().map_err(io)?,
                            name,
                            segments: candidate,
                            stat: None,
                            created,
                        });
                    }
                    if !last && lookup.create_parents {
                        match directory.mkdir(&name, DEFAULT_DIRECTORY_MODE) {
                            Ok(()) => created.push(candidate),
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                            Err(error) => return Err(io(error)),
                        }
                        directory.sync().map_err(io)?;
                        pending.push_front(name);
                        continue;
                    }
                    return Err(io(error));
                }
                Err(error) => return Err(io(error)),
            };
            self.admit(&files, &stat, subject)?;
            if stat.kind == environment_protocol::data::fs::FileKind::Symlink
                && (!last || lookup.follow_final)
            {
                if lookup.no_symlinks {
                    return Err(ProtocolError::failed_precondition(
                        PreconditionReason::SymlinkLoop,
                        format!("{subject}: a symlink replaced a recorded entry"),
                    ));
                }
                follows += 1;
                if follows > MAX_SYMLINK_FOLLOWS {
                    return Err(ProtocolError::failed_precondition(
                        PreconditionReason::SymlinkLoop,
                        format!("{subject}: too many levels of symbolic links"),
                    ));
                }
                let target = directory.read_link(&name).map_err(io)?;
                let target = Path::new(&target);
                let components: Vec<OsString> = if target.is_absolute() {
                    let Ok(rest) = target.strip_prefix(&self.root) else {
                        return Err(outside_root(subject));
                    };
                    stack.truncate(1);
                    segments.clear();
                    raw_components(rest)
                } else {
                    raw_components(target)
                };
                for component in components.into_iter().rev() {
                    pending.push_front(component);
                }
                continue;
            }
            if last {
                return Ok(Resolved {
                    parent: directory.try_clone().map_err(io)?,
                    name,
                    segments: candidate,
                    stat: Some(stat),
                    created,
                });
            }
            if !stat.is_dir() {
                return Err(ProtocolError::failed_precondition(
                    PreconditionReason::NotDirectory,
                    format!("{subject}: a path component is not a directory"),
                ));
            }
            let child = directory.child(&name).map_err(io)?;
            stack.push(child);
            segments.push(name);
        }
        // The path ended on a directory already open: the root, or a
        // directory reached through `..` or a link ending in one.
        let current = stack.pop().expect("the root stays on the stack");
        let stat = EntryStat::of_directory(&current).map_err(io)?;
        self.admit(&files, &stat, subject)?;
        match (stack.pop(), segments.last()) {
            (Some(parent), Some(name)) => Ok(Resolved {
                parent,
                name: name.clone(),
                segments,
                stat: Some(stat),
                created,
            }),
            _ => Ok(Resolved {
                parent: current,
                name: OsString::from(SELF),
                segments,
                stat: Some(stat),
                created,
            }),
        }
    }
}

/// Components of a link target, keeping `.` and `..` for the walk to apply
/// against the open directories.
fn raw_components(path: &Path) -> Vec<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str()
            .as_bytes()
            .split(|byte| *byte == b'/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| OsStr::from_bytes(segment).to_owned())
            .collect()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        path.as_os_str()
            .encode_wide()
            .collect::<Vec<_>>()
            .split(|c| matches!(*c, 47 | 92))
            .filter(|segment| !segment.is_empty())
            .map(OsString::from_wide)
            .collect()
    }
}

pub(crate) fn outside_root(subject: &dyn std::fmt::Display) -> ProtocolError {
    ProtocolError::permission_denied(
        Some(PermissionReason::OutsideRoot),
        format!("{subject} is outside the filesystem root"),
    )
}

pub(crate) fn daemon_state(subject: &dyn std::fmt::Display) -> ProtocolError {
    ProtocolError::permission_denied(
        Some(PermissionReason::DaemonState),
        format!("{subject} addresses the daemon state directory"),
    )
}

/// A kernel filesystem or the registration key. Reported under the reason
/// of the daemon's own state: to a caller it is the same kind of refusal,
/// and one reason keeps the protocol as it is.
fn protected_location(subject: &dyn std::fmt::Display) -> ProtocolError {
    ProtocolError::permission_denied(
        Some(PermissionReason::DaemonState),
        format!(
            "{subject} addresses a kernel filesystem or the daemon's registration key, \
             which file operations never reach"
        ),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use environment_protocol::error::{ErrorData, ProtocolErrorKind};

    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        outside: PathBuf,
        root: PathBuf,
        confinement: Confinement,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("src/lib.rs"), "lib").unwrap();
        std::fs::write(outside.join("secret"), "secret").unwrap();
        let confinement = Confinement::new(root.clone(), root.join(".state")).unwrap();
        Fixture {
            _temp: temp,
            outside,
            root,
            confinement,
        }
    }

    fn path(root: &Path, relative: &str) -> EnvironmentPath {
        EnvironmentPath::new(format!("{}/{relative}", root.display())).unwrap()
    }

    fn permission_reason(error: &ProtocolError) -> Option<PermissionReason> {
        match error.data {
            ErrorData::PermissionDenied { reason } => reason,
            _ => None,
        }
    }

    #[test]
    fn paths_outside_the_root_are_refused_before_any_lookup() {
        let fixture = fixture();
        let error = fixture
            .confinement
            .resolve(
                &EnvironmentPath::new(fixture.outside.join("secret").to_str().unwrap()).unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            permission_reason(&error),
            Some(PermissionReason::OutsideRoot)
        );
    }

    #[test]
    fn in_root_links_are_followed_and_escaping_links_refused() {
        let fixture = fixture();
        symlink("src", fixture.root.join("inside")).unwrap();
        // Relative to the link's directory: up to the root, then back down.
        symlink("../../src/lib.rs", fixture.root.join("src/deep/up")).unwrap();
        symlink(&fixture.outside, fixture.root.join("absolute-escape")).unwrap();
        symlink("../outside/secret", fixture.root.join("relative-escape")).unwrap();
        symlink(
            fixture.root.join("src"),
            fixture.root.join("absolute-inside"),
        )
        .unwrap();

        assert_eq!(
            fixture
                .confinement
                .resolve(&path(&fixture.root, "inside/lib.rs"))
                .unwrap(),
            fixture.root.join("src/lib.rs")
        );
        assert_eq!(
            fixture
                .confinement
                .resolve(&path(&fixture.root, "src/deep/up"))
                .unwrap(),
            fixture.root.join("src/lib.rs")
        );
        assert_eq!(
            fixture
                .confinement
                .resolve_directory(&path(&fixture.root, "absolute-inside"))
                .unwrap(),
            fixture.root.join("src")
        );
        for escape in [
            "absolute-escape/secret",
            "relative-escape",
            "absolute-escape",
        ] {
            let error = fixture
                .confinement
                .resolve(&path(&fixture.root, escape))
                .unwrap_err();
            assert_eq!(
                permission_reason(&error),
                Some(PermissionReason::OutsideRoot),
                "{escape}"
            );
        }
    }

    #[test]
    fn the_state_directory_is_never_addressed() {
        let fixture = fixture();
        std::fs::write(fixture.root.join(".state/daemon-key"), "key").unwrap();
        symlink(".state", fixture.root.join("state-link")).unwrap();
        for subject in [".state", ".state/daemon-key", "state-link/daemon-key"] {
            let error = fixture
                .confinement
                .resolve(&path(&fixture.root, subject))
                .unwrap_err();
            assert_eq!(
                permission_reason(&error),
                Some(PermissionReason::DaemonState),
                "{subject}"
            );
        }
        let root = fixture
            .confinement
            .lookup(fixture.confinement.root_path(), Lookup::NO_FOLLOW)
            .unwrap();
        assert!(root.is_root());
        assert!(fixture.confinement.contains_state(&root.stat.unwrap()));
        let src = fixture
            .confinement
            .lookup(&path(&fixture.root, "src"), Lookup::NO_FOLLOW)
            .unwrap();
        assert!(!fixture.confinement.contains_state(&src.stat.unwrap()));
    }

    #[test]
    fn the_state_directory_is_recognized_by_identity_not_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("root");
        std::fs::create_dir_all(root.join("local/state")).unwrap();
        let confinement = Confinement::new(root.clone(), root.join("local/state/envd")).unwrap();
        std::fs::write(root.join("local/state/envd/daemon-key"), "key").unwrap();
        // Another name for the same directories: no recorded spelling of
        // the state directory matches it any more.
        std::fs::rename(root.join("local"), root.join("moved")).unwrap();
        // Where the filesystem folds case (APFS), other spellings reach the
        // same entries; elsewhere they are missing.
        for subject in [
            "moved/state/envd/daemon-key",
            "moved/state/envd",
            "MOVED/STATE/ENVD/daemon-key",
        ] {
            match confinement.resolve(&path(&root, subject)) {
                Err(error) if error.kind() == ProtocolErrorKind::NotFound => {
                    assert!(subject.starts_with("MOVED"), "{subject}");
                }
                Err(error) => assert_eq!(
                    permission_reason(&error),
                    Some(PermissionReason::DaemonState),
                    "{subject}"
                ),
                Ok(resolved) => panic!("{subject} resolved to {}", resolved.display()),
            }
        }
        for ancestor in ["moved", "moved/state"] {
            let resolved = confinement
                .lookup(&path(&root, ancestor), Lookup::NO_FOLLOW)
                .unwrap();
            assert!(
                confinement
                    .refuse_containing_state(&resolved, &ancestor)
                    .is_err(),
                "{ancestor}"
            );
            assert_eq!(
                confinement.holds_state(&resolved.stat.unwrap()),
                ancestor == "moved/state"
            );
        }
    }

    /// A root with a key file beside the sources, refused by its path.
    fn key_fixture() -> (tempfile::TempDir, PathBuf, Confinement) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("root");
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("secrets/key"), "lsrk_secret").unwrap();
        std::fs::write(root.join("secrets/notes"), "notes").unwrap();
        let mut protected = Protected::default();
        protected.refuse_file(&root.join("secrets/key"));
        let state = root.parent().unwrap().join("state");
        let confinement = Confinement::protecting(root.clone(), state, protected).unwrap();
        (temp, root, confinement)
    }

    fn refused(confinement: &Confinement, root: &Path, subject: &str) {
        let error = confinement.resolve(&path(root, subject)).unwrap_err();
        assert_eq!(
            permission_reason(&error),
            Some(PermissionReason::DaemonState),
            "{subject}"
        );
    }

    #[test]
    fn a_refused_filesystem_is_never_addressed_under_any_root() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        // `/dev` stands in for a kernel filesystem: a mount of its own on
        // every Unix, reachable without privileges.
        assert_ne!(
            std::fs::metadata("/dev").unwrap().dev(),
            std::fs::metadata(&base).unwrap().dev(),
            "this test needs /dev to be a filesystem of its own"
        );
        std::fs::write(base.join("file"), "file").unwrap();
        symlink("/dev/null", base.join("link")).unwrap();
        symlink("/dev", base.join("directory-link")).unwrap();
        let mut protected = Protected::default();
        protected.refuse_filesystem_at(Path::new("/dev")).unwrap();
        let confinement =
            Confinement::protecting("/".into(), base.join("state"), protected).unwrap();

        let slash = Path::new("");
        let workspace = base.to_str().unwrap().trim_start_matches('/').to_owned();
        for subject in [
            "dev".to_owned(),
            "dev/null".to_owned(),
            format!("{workspace}/link"),
            format!("{workspace}/directory-link"),
            format!("{workspace}/directory-link/null"),
        ] {
            refused(&confinement, slash, &subject);
        }
        // A missing name there is refused at the mount, not reported missing.
        let error = confinement
            .lookup(
                &path(slash, "dev/absent"),
                Lookup::FOLLOW.allowing_missing(),
            )
            .unwrap_err();
        assert_eq!(
            permission_reason(&error),
            Some(PermissionReason::DaemonState)
        );
        // The link itself is an ordinary entry of the workspace.
        let link = confinement
            .lookup(&path(&base, "link"), Lookup::NO_FOLLOW)
            .unwrap();
        assert_eq!(
            link.stat.unwrap().kind,
            environment_protocol::data::fs::FileKind::Symlink
        );
        assert_eq!(
            confinement.resolve(&path(&base, "file")).unwrap(),
            base.join("file")
        );
        // The same root without the rule reaches the device directory.
        let open = Confinement::new("/".into(), base.join("state")).unwrap();
        assert!(open.resolve(&path(slash, "dev/null")).is_ok());
    }

    #[test]
    fn the_registration_key_is_refused_under_every_name_that_reaches_it() {
        let (_temp, root, confinement) = key_fixture();
        symlink("secrets/key", root.join("link")).unwrap();
        symlink("secrets", root.join("directory-link")).unwrap();
        std::fs::hard_link(root.join("secrets/key"), root.join("src/copy")).unwrap();
        symlink("../secrets/key", root.join("src/up")).unwrap();
        for subject in [
            "secrets/key",
            "link",
            "directory-link/key",
            "src/copy",
            "src/up",
        ] {
            refused(&confinement, &root, subject);
        }
        // Where the filesystem folds case (APFS), another spelling reaches
        // the same file; elsewhere it is missing.
        match confinement.resolve(&path(&root, "SECRETS/KEY")) {
            Err(error) if error.kind() == ProtocolErrorKind::NotFound => {}
            Err(error) => assert_eq!(
                permission_reason(&error),
                Some(PermissionReason::DaemonState)
            ),
            Ok(resolved) => panic!("another spelling resolved to {}", resolved.display()),
        }
        // Its neighbours and the directory itself stay addressable.
        assert_eq!(
            confinement.resolve(&path(&root, "secrets/notes")).unwrap(),
            root.join("secrets/notes")
        );
        assert!(
            confinement
                .resolve_directory(&path(&root, "secrets"))
                .is_ok()
        );
    }

    #[test]
    fn a_replaced_key_moves_the_rule_to_the_new_file() {
        let (_temp, root, confinement) = key_fixture();
        std::fs::hard_link(root.join("secrets/key"), root.join("src/old")).unwrap();
        refused(&confinement, &root, "src/old");
        // Rotation as a writer does it: a new file renamed over the path.
        std::fs::write(root.join("secrets/next"), "lsrk_rotated").unwrap();
        std::fs::rename(root.join("secrets/next"), root.join("secrets/key")).unwrap();
        refused(&confinement, &root, "secrets/key");
        // No inode was recorded: the file that is no longer the key is an
        // ordinary file again.
        assert_eq!(
            confinement.resolve(&path(&root, "src/old")).unwrap(),
            root.join("src/old")
        );
        // With the key gone nothing is refused, and a key written there
        // later is refused again.
        std::fs::remove_file(root.join("secrets/key")).unwrap();
        let missing = confinement
            .resolve(&path(&root, "secrets/key"))
            .unwrap_err();
        assert_eq!(missing.kind(), ProtocolErrorKind::NotFound);
        std::fs::write(root.join("secrets/key"), "lsrk_again").unwrap();
        refused(&confinement, &root, "secrets/key");
    }

    #[test]
    fn a_directory_holding_the_key_is_refused_like_one_holding_state() {
        let (_temp, root, confinement) = key_fixture();
        for (subject, holds) in [("secrets", true), ("src", false)] {
            let resolved = confinement
                .lookup(&path(&root, subject), Lookup::NO_FOLLOW)
                .unwrap();
            let refusal = confinement.refuse_containing_state(&resolved, &subject);
            assert_eq!(refusal.is_err(), holds, "{subject}");
            if let Err(error) = refusal {
                assert_eq!(
                    permission_reason(&error),
                    Some(PermissionReason::DaemonState)
                );
            }
        }
        let whole = confinement
            .lookup(confinement.root_path(), Lookup::NO_FOLLOW)
            .unwrap();
        assert!(
            confinement
                .refuse_containing_state(&whole, &"root")
                .is_err()
        );
        // A missing destination holds nothing.
        let absent = confinement
            .lookup(
                &path(&root, "src/absent"),
                Lookup::NO_FOLLOW.allowing_missing(),
            )
            .unwrap();
        assert!(
            confinement
                .refuse_containing_state(&absent, &"absent")
                .is_ok()
        );
    }

    #[test]
    fn loops_and_missing_entries_are_typed() {
        let fixture = fixture();
        symlink("b", fixture.root.join("a")).unwrap();
        symlink("a", fixture.root.join("b")).unwrap();
        let error = fixture
            .confinement
            .resolve(&path(&fixture.root, "a"))
            .unwrap_err();
        assert!(matches!(
            error.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::SymlinkLoop
            }
        ));
        let error = fixture
            .confinement
            .resolve(&path(&fixture.root, "missing"))
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::NotFound);
        let error = fixture
            .confinement
            .resolve_directory(&path(&fixture.root, "src/lib.rs"))
            .unwrap_err();
        assert!(matches!(
            error.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::NotDirectory
            }
        ));
    }

    #[test]
    fn dangling_in_root_links_resolve_to_their_missing_target() {
        let fixture = fixture();
        symlink("src/new.rs", fixture.root.join("dangling")).unwrap();
        let resolved = fixture
            .confinement
            .lookup(
                &path(&fixture.root, "dangling"),
                Lookup::FOLLOW.allowing_missing(),
            )
            .unwrap();
        assert!(resolved.stat.is_none());
        assert_eq!(
            fixture.confinement.host_path(&resolved.segments),
            fixture.root.join("src/new.rs")
        );
    }
}
