use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path},
};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};

pub struct Directory(File);
#[derive(Clone, Debug)]
pub struct Observation(Metadata);
impl Observation {
    pub fn is_dir(&self) -> bool {
        self.0.is_dir()
    }
    pub fn size(&self) -> u64 {
        self.0.len()
    }
    pub fn executable(&self) -> bool {
        self.0.mode() & 0o111 != 0
    }
    pub fn matches(&self, other: &Self) -> bool {
        let (a, b) = (&self.0, &other.0);
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.mode() == b.mode()
            && a.len() == b.len()
            && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
}
fn c(name: &str) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}
fn check(meta: Metadata) -> io::Result<Observation> {
    if meta.is_file() || meta.is_dir() {
        Ok(Observation(meta))
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "scoped filesystem access does not support symlinks or special files",
        ))
    }
}
pub fn observe(file: &File) -> io::Result<Observation> {
    check(file.metadata()?)
}
pub fn set_executable(file: &File, executable: bool) -> io::Result<()> {
    file.set_permissions(std::fs::Permissions::from_mode(if executable {
        0o755
    } else {
        0o644
    }))
}
/// Streaming directory enumeration; cleanup never collects an entire retired tree.
struct Entries(fs::Dir);
impl Iterator for Entries {
    type Item = io::Result<CString>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.find_map(|entry| match entry {
            Ok(entry) => {
                let name = entry.file_name();
                if name.to_bytes() == b"." || name.to_bytes() == b".." {
                    None
                } else {
                    Some(Ok(name.to_owned()))
                }
            }
            Err(error) => Some(Err(error.into())),
        })
    }
}
fn entries(file: &File) -> io::Result<Entries> {
    // Open a separate description so enumeration never shares the caller's offset.
    let fd = fs::openat(
        file,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    Ok(Entries(fs::Dir::new(fd)?))
}

pub fn is_path_violation(error: &io::Error) -> bool {
    matches!(
        rustix::io::Errno::from_io_error(error),
        Some(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR)
    )
}
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}
pub fn sync_directory(path: &Path) -> io::Result<()> {
    Directory::anchor(path)?.sync()
}

impl Directory {
    pub fn anchor(root: &Path) -> io::Result<Self> {
        // Configured roots may contain platform aliases (/var on macOS). Resolve this
        // administrator-owned anchor once; user selections are always opened relative to it.
        let root = root.canonicalize()?;
        let mut dir = Self(
            fs::open(
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into(),
        );
        for part in root.components() {
            if let Component::Normal(name) = part {
                let name = name
                    .to_str()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                dir = dir.child(name)?;
            }
        }
        Ok(dir)
    }
    pub fn sync(&self) -> io::Result<()> {
        self.0.sync_all()
    }
    pub fn clone_dir(&self) -> io::Result<Self> {
        Ok(Self(self.0.try_clone()?))
    }
    pub fn child(&self, name: &str) -> io::Result<Self> {
        self.child_name(name)
    }
    fn child_name(&self, name: impl rustix::path::Arg) -> io::Result<Self> {
        Ok(Self(
            fs::openat(
                &self.0,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into(),
        ))
    }
    pub fn parent(&self, path: &str) -> io::Result<(Self, String)> {
        self.parent_with_creation(path, false)
    }
    /// Prepare a destination without following symlinks in existing or raced-in parents.
    pub fn ensure_parent(&self, path: &str) -> io::Result<(Self, String)> {
        self.parent_with_creation(path, true)
    }
    fn parent_with_creation(&self, path: &str, create_missing: bool) -> io::Result<(Self, String)> {
        let mut parts = path.split('/').collect::<Vec<_>>();
        let name = parts.pop().unwrap_or("").to_owned();
        let mut dir = self.clone_dir()?;
        for part in parts {
            dir = match dir.child(part) {
                Ok(child) => child,
                Err(error) if create_missing && error.kind() == io::ErrorKind::NotFound => {
                    let child = match dir.mkdir(part, false) {
                        Ok(child) => child,
                        // Another creator may win the mkdir race. Reopen with the same
                        // directory-only, no-follow checks used for existing parents.
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            dir.child(part)?
                        }
                        Err(error) => return Err(error),
                    };
                    dir.sync()?;
                    child
                }
                Err(error) => return Err(error),
            };
        }
        Ok((dir, if name.is_empty() { ".".into() } else { name }))
    }
    pub fn open(&self, path: &str) -> io::Result<File> {
        self.open_kind(path, false)
    }
    pub fn metadata(&self, path: &str) -> io::Result<File> {
        self.open_kind(path, true)
    }
    fn open_kind(&self, path: &str, metadata_only: bool) -> io::Result<File> {
        let (dir, name) = self.parent(path)?;
        #[cfg(target_os = "linux")]
        let access = if metadata_only {
            OFlags::PATH
        } else {
            OFlags::RDONLY
        };
        #[cfg(target_os = "macos")]
        let access = if metadata_only {
            // Rustix does not name the macOS metadata-only flag.
            OFlags::from_bits_retain(libc::O_EVTONLY as u32)
        } else {
            OFlags::RDONLY
        };
        let file = fs::openat(
            &dir.0,
            &name,
            access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        observe(&file)?;
        Ok(file)
    }
    /// Inspect replacement targets without requiring read permission on their content.
    pub fn target_exists(&self, name: &str) -> io::Result<bool> {
        let stat = match fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if !matches!(
            FileType::from_raw_mode(stat.st_mode),
            FileType::Directory | FileType::RegularFile
        ) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "target is a symlink or special file",
            ));
        }
        Ok(true)
    }
    pub fn names(file: &File, limit: usize) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for name in entries(file)? {
            if names.len() >= limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "directory entry limit exceeded",
                ));
            }
            names.push(
                name?.into_string().map_err(|_| {
                    io::Error::new(io::ErrorKind::Unsupported, "non-UTF-8 filename")
                })?,
            );
        }
        names.sort();
        Ok(names)
    }
    pub fn mkdir(&self, name: &str, private: bool) -> io::Result<Self> {
        fs::mkdirat(
            &self.0,
            name,
            Mode::from_raw_mode(if private { 0o700 } else { 0o755 }),
        )?;
        self.child(name)
    }
    pub fn create(&self, path: &str) -> io::Result<File> {
        let (dir, name) = self.parent(path)?;
        Ok(fs::openat(
            &dir.0,
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?
        .into())
    }
    pub fn publish(&self, stage: &Self, target: &str, replace: bool) -> io::Result<bool> {
        // Rustix handles flagged renames on both Linux libc variants and macOS.
        let rename = |exchange: bool| {
            fs::renameat_with(
                &stage.0,
                "tree",
                &self.0,
                target,
                if exchange {
                    fs::RenameFlags::EXCHANGE
                } else {
                    fs::RenameFlags::NOREPLACE
                },
            )
            .map_err(io::Error::from)
        };
        match rename(false) {
            Ok(()) => Ok(false),
            Err(error) if replace && error.kind() == io::ErrorKind::AlreadyExists => {
                rename(true)?;
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }
    pub fn remove_tree(&self, name: &str) -> io::Result<()> {
        struct Frame {
            name: CString,
            directory: Directory,
            entries: Entries,
        }
        let name = c(name)?;
        let unlink = |parent: &Directory, name: &CStr, flags: AtFlags| -> io::Result<()> {
            Ok(fs::unlinkat(&parent.0, name, flags)?)
        };
        let directory = match self.child_name(&name) {
            Ok(dir) => dir,
            Err(_) => return unlink(self, &name, AtFlags::empty()),
        };
        let mut stack = vec![Frame {
            name,
            entries: entries(&directory.0)?,
            directory,
        }];
        while let Some(frame) = stack.last_mut() {
            if let Some(name) = frame.entries.next() {
                let name = name?;
                match frame.directory.child_name(&name) {
                    Ok(directory) => {
                        if stack.len() >= 256 {
                            return Err(io::Error::new(
                                io::ErrorKind::Unsupported,
                                "retirement cleanup depth limit exceeded",
                            ));
                        }
                        stack.push(Frame {
                            name,
                            entries: entries(&directory.0)?,
                            directory,
                        });
                    }
                    Err(_) => unlink(&frame.directory, &name, AtFlags::empty())?,
                }
            } else {
                let frame = stack.pop().unwrap();
                let parent = stack.last().map_or(self, |frame| &frame.directory);
                unlink(parent, &frame.name, AtFlags::REMOVEDIR)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_enumerations_have_independent_offsets_and_enforce_limits() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(root.path().join(name), name).unwrap();
        }
        let dir = Directory::anchor(root.path()).unwrap();
        let mut first = entries(&dir.0).unwrap();
        let mut observed = vec![first.next().unwrap().unwrap().into_string().unwrap()];
        assert_eq!(Directory::names(&dir.0, 3).unwrap(), ["a", "b", "c"]);
        observed.extend(first.map(|name| name.unwrap().into_string().unwrap()));
        observed.sort();
        assert_eq!(observed, ["a", "b", "c"]);
        assert_eq!(
            Directory::names(&dir.0, 2).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(Directory::names(&dir.0, 3).unwrap(), ["a", "b", "c"]);
    }

    #[test]
    fn cleanup_removes_nested_trees_without_following_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "untouched").unwrap();
        let dir = Directory::anchor(root.path()).unwrap();
        let retired = dir.mkdir("retired", true).unwrap();
        retired.mkdir("nested", true).unwrap();
        std::fs::write(root.path().join("retired/nested/file"), "content").unwrap();
        symlink(outside.path(), root.path().join("retired/link")).unwrap();
        symlink("missing", root.path().join("retired/dangling")).unwrap();
        dir.remove_tree("retired").unwrap();
        assert!(!root.path().join("retired").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("keep")).unwrap(),
            "untouched"
        );
    }

    // macOS filesystems reject invalid UTF-8 at creation time.
    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_handles_non_utf8_names() {
        use std::os::unix::ffi::OsStrExt;

        let root = tempfile::tempdir().unwrap();
        let dir = Directory::anchor(root.path()).unwrap();
        dir.mkdir("retired", true).unwrap();
        let raw_path = root
            .path()
            .join("retired")
            .join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
        std::fs::create_dir(&raw_path).unwrap();
        std::fs::write(
            raw_path.join(std::ffi::OsStr::from_bytes(b"file-\xfe")),
            "data",
        )
        .unwrap();
        dir.remove_tree("retired").unwrap();
        assert!(!root.path().join("retired").exists());
    }

    #[test]
    fn publication_refuses_overwrite_and_exchanges_replaced_files_and_trees() {
        for directory in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let parent = Directory::anchor(root.path()).unwrap();
            let stage = parent.mkdir("stage", true).unwrap();
            let staged = root.path().join("stage/tree");
            let target = root.path().join("target");
            let write = |path: &Path, bytes: &[u8]| {
                if directory {
                    std::fs::create_dir(path).unwrap();
                    std::fs::write(path.join("content"), bytes).unwrap();
                } else {
                    std::fs::write(path, bytes).unwrap();
                }
            };
            let read = |path: &Path| {
                std::fs::read(if directory {
                    path.join("content")
                } else {
                    path.to_owned()
                })
                .unwrap()
            };

            write(&staged, b"original");
            assert!(!parent.publish(&stage, "target", false).unwrap());
            assert_eq!(read(&target), b"original");
            assert!(!staged.exists());

            write(&staged, b"replacement");
            let error = parent.publish(&stage, "target", false).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(read(&target), b"original");
            assert_eq!(read(&staged), b"replacement");

            assert!(parent.publish(&stage, "target", true).unwrap());
            assert_eq!(read(&target), b"replacement");
            // Exchange retains the old target in staging for later cleanup.
            assert_eq!(read(&staged), b"original");
        }
    }
}
