//! Descriptor-relative filesystem primitives. Every operation names an entry
//! by a directory descriptor and one name, never by a host path, so nothing
//! here follows a symlink implicitly. Wire paths, inventories and receipts
//! never contain descriptors or inode numbers; only [`FileVersion`] tokens
//! are derived from them.

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read},
    os::{
        fd::OwnedFd,
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::Path,
};

use environment_protocol::data::fs::{FileKind, FileVersion};
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags, RenameFlags};

/// An open directory. Children are opened beneath it without following
/// symlinks.
#[derive(Debug)]
pub(crate) struct Directory(OwnedFd);

/// The entry a directory descriptor itself names.
pub(crate) const SELF: &str = ".";

impl Directory {
    /// Open an administrator-configured directory by its host path. Only the
    /// filesystem root and the transfer journal are opened this way; every
    /// wire path is resolved beneath the root's descriptor.
    pub fn open_host(path: &Path) -> io::Result<Self> {
        Ok(Self(rfs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self(self.0.try_clone()?))
    }

    /// Open the child directory `name`; a symlink or non-directory fails
    /// (`ELOOP` or `ENOTDIR`).
    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        Ok(Self(rfs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    /// `lstat` of `name` (`.` for the directory itself).
    pub fn stat(&self, name: &OsStr) -> io::Result<EntryStat> {
        Ok(EntryStat::from_stat(&rfs::statat(
            &self.0,
            name,
            AtFlags::SYMLINK_NOFOLLOW,
        )?))
    }

    /// `lstat` plus the birth time where the filesystem records one.
    pub fn stat_with_birth(&self, name: &OsStr) -> io::Result<EntryStat> {
        let stat = rfs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let mut entry = EntryStat::from_stat(&stat);
        entry.birth_ms = birth_ms(&self.0, name, &stat);
        Ok(entry)
    }

    pub fn read_link(&self, name: &OsStr) -> io::Result<OsString> {
        let target = rfs::readlinkat(&self.0, name, Vec::new())?;
        Ok(OsString::from_vec(target.into_bytes()))
    }

    /// Open `name` for reading without following a symlink and without
    /// blocking on a FIFO or device; callers `fstat` before reading.
    pub fn open_read(&self, name: &OsStr) -> io::Result<File> {
        Ok(rfs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into())
    }

    /// Create `name` exclusively, never following a symlink.
    pub fn create_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        Ok(rfs::openat(
            &self.0,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode as _),
        )?
        .into())
    }

    pub fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        Ok(rfs::mkdirat(&self.0, name, Mode::from_raw_mode(mode as _))?)
    }

    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        Ok(rfs::unlinkat(&self.0, name, AtFlags::empty())?)
    }

    pub fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        Ok(rfs::unlinkat(&self.0, name, AtFlags::REMOVEDIR)?)
    }

    /// Rename `from` in `self` to `to` in `destination`.
    pub fn rename(
        &self,
        from: &OsStr,
        destination: &Directory,
        to: &OsStr,
        mode: RenameMode,
    ) -> io::Result<()> {
        match mode {
            RenameMode::Replace => rfs::renameat(&self.0, from, &destination.0, to)?,
            RenameMode::NoReplace => {
                rfs::renameat_with(&self.0, from, &destination.0, to, RenameFlags::NOREPLACE)?
            }
            RenameMode::Exchange => {
                rfs::renameat_with(&self.0, from, &destination.0, to, RenameFlags::EXCHANGE)?
            }
        }
        Ok(())
    }

    /// Every name except `.` and `..`, unsorted. A separate open file
    /// description is used, so enumerations never share an offset.
    pub fn names(&self) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in self.entries()? {
            names.push(entry?);
        }
        Ok(names)
    }

    fn entries(&self) -> io::Result<Entries> {
        let fd = rfs::openat(
            &self.0,
            SELF,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Entries(rfs::Dir::new(fd)?))
    }

    pub fn sync(&self) -> io::Result<()> {
        Ok(rfs::fsync(&self.0)?)
    }

    /// Remove `name` and everything below it without following symlinks.
    /// Streams each directory, so a large tree is never collected at once.
    pub fn remove_tree(&self, name: &OsStr) -> io::Result<()> {
        struct Frame {
            name: OsString,
            directory: Directory,
            entries: Entries,
        }
        let directory = match self.child(name) {
            Ok(directory) => directory,
            Err(error) if is_not_directory(&error) => return self.unlink(name),
            Err(error) => return Err(error),
        };
        let mut stack = vec![Frame {
            name: name.to_owned(),
            entries: directory.entries()?,
            directory,
        }];
        while let Some(frame) = stack.last_mut() {
            if let Some(child) = frame.entries.next() {
                let child = child?;
                match frame.directory.child(&child) {
                    Ok(directory) => {
                        if stack.len() >= MAX_REMOVE_DEPTH {
                            return Err(io::Error::other("directory tree is too deep to remove"));
                        }
                        stack.push(Frame {
                            name: child,
                            entries: directory.entries()?,
                            directory,
                        });
                    }
                    Err(error) if is_not_directory(&error) => frame.directory.unlink(&child)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            } else {
                let frame = stack.pop().expect("frame exists");
                let parent = stack.last().map_or(self, |frame| &frame.directory);
                match parent.rmdir(&frame.name) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    result => result?,
                }
            }
        }
        Ok(())
    }
}

const MAX_REMOVE_DEPTH: usize = 4096;

/// How [`Directory::rename`] treats an existing destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameMode {
    /// Replace a file (`rename(2)`).
    Replace,
    /// Fail with `EEXIST` when the destination exists.
    NoReplace,
    /// Swap source and destination atomically.
    Exchange,
}

/// `ELOOP` (a symlink met with `O_NOFOLLOW`) or `ENOTDIR`.
pub(crate) fn is_not_directory(error: &io::Error) -> bool {
    matches!(
        rustix::io::Errno::from_io_error(error),
        Some(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR)
    )
}

struct Entries(rfs::Dir);

impl Iterator for Entries {
    type Item = io::Result<OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.find_map(|entry| match entry {
            Ok(entry) => {
                let name = entry.file_name().to_bytes();
                (name != b"." && name != b"..").then(|| Ok(OsString::from_vec(name.to_vec())))
            }
            Err(error) => Some(Err(error.into())),
        })
    }
}

/// What `lstat` or `fstat` reported about one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryStat {
    pub kind: FileKind,
    pub mode: u32,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
    pub birth_ms: Option<i64>,
}

impl EntryStat {
    #[allow(clippy::unnecessary_cast)]
    pub fn from_stat(stat: &rfs::Stat) -> Self {
        let kind = match FileType::from_raw_mode(stat.st_mode as _) {
            FileType::RegularFile => FileKind::File,
            FileType::Directory => FileKind::Directory,
            FileType::Symlink => FileKind::Symlink,
            _ => FileKind::Other,
        };
        Self {
            kind,
            mode: stat.st_mode as u32 & 0o7777,
            size: stat.st_size as u64,
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
            modified: (stat.st_mtime as i64, stat.st_mtime_nsec as i64),
            changed: (stat.st_ctime as i64, stat.st_ctime_nsec as i64),
            birth_ms: None,
        }
    }

    pub fn of_file(file: &File) -> io::Result<Self> {
        Ok(Self::from_stat(&rfs::fstat(file)?))
    }

    pub fn of_directory(directory: &Directory) -> io::Result<Self> {
        Ok(Self::from_stat(&rfs::fstat(&directory.0)?))
    }

    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory
    }

    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }

    pub fn executable(&self) -> bool {
        self.mode & 0o111 != 0
    }

    pub fn modified_ms(&self) -> i64 {
        to_ms(self.modified)
    }

    /// Device and inode.
    pub fn identity(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }

    /// Same inode with the same size, modification and change times: the
    /// content observed earlier is still the content.
    pub fn unchanged(&self, other: &Self) -> bool {
        self.dev == other.dev
            && self.ino == other.ino
            && self.size == other.size
            && self.modified == other.modified
            && self.changed == other.changed
    }

    /// The opaque `version` token: identity, size and both timestamps.
    pub fn version(&self) -> FileVersion {
        FileVersion::new(format!(
            "v1-{:x}-{:x}-{:x}-{:x}.{:x}-{:x}.{:x}",
            self.dev,
            self.ino,
            self.size,
            self.modified.0 as u64,
            self.modified.1 as u64,
            self.changed.0 as u64,
            self.changed.1 as u64,
        ))
        .expect("version tokens are short and printable")
    }
}

fn to_ms((seconds, nanoseconds): (i64, i64)) -> i64 {
    seconds
        .saturating_mul(1000)
        .saturating_add(nanoseconds / 1_000_000)
}

#[cfg(target_os = "linux")]
fn birth_ms(directory: &OwnedFd, name: &OsStr, _: &rfs::Stat) -> Option<i64> {
    let statx = rfs::statx(
        directory,
        name,
        AtFlags::SYMLINK_NOFOLLOW,
        rfs::StatxFlags::BTIME,
    )
    .ok()?;
    rfs::StatxFlags::from_bits_retain(statx.stx_mask)
        .contains(rfs::StatxFlags::BTIME)
        .then(|| to_ms((statx.stx_btime.tv_sec, statx.stx_btime.tv_nsec as i64)))
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
#[allow(clippy::unnecessary_cast)]
fn birth_ms(_: &OwnedFd, _: &OsStr, stat: &rfs::Stat) -> Option<i64> {
    // Filesystems without birth times report zero or a negative value.
    (stat.st_birthtime > 0)
        .then(|| to_ms((stat.st_birthtime as i64, stat.st_birthtime_nsec as i64)))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
fn birth_ms(_: &OwnedFd, _: &OsStr, _: &rfs::Stat) -> Option<i64> {
    None
}

/// Read at most `max` bytes from the current position.
pub(crate) fn read_up_to(file: &mut File, max: u64) -> io::Result<Vec<u8>> {
    let mut data = Vec::with_capacity(max.min(1024 * 1024) as usize);
    file.take(max).read_to_end(&mut data)?;
    Ok(data)
}

/// A name as UTF-8, or `None` when it is not representable on the wire.
pub(crate) fn utf8_name(name: &OsStr) -> Option<&str> {
    std::str::from_utf8(name.as_bytes()).ok()
}

/// Set the permission bits of an open file.
pub(crate) fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    Ok(rfs::fchmod(file, Mode::from_raw_mode(mode as _))?)
}

/// The mode with an execute bit added wherever a read bit is set.
pub(crate) fn with_execute_bits(mode: u32) -> u32 {
    mode | ((mode & 0o444) >> 2)
}

/// Mode of a file the daemon creates.
pub(crate) const DEFAULT_FILE_MODE: u32 = 0o644;
/// Mode of a directory the daemon creates.
pub(crate) const DEFAULT_DIRECTORY_MODE: u32 = 0o755;
/// Mode of the daemon's own staging directories.
pub(crate) const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
/// Mode of a staged file before its final mode is set.
pub(crate) const PRIVATE_FILE_MODE: u32 = 0o600;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_streams_nested_trees_without_following_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "untouched").unwrap();
        std::fs::create_dir_all(root.path().join("retired/nested")).unwrap();
        std::fs::write(root.path().join("retired/nested/file"), "content").unwrap();
        symlink(outside.path(), root.path().join("retired/link")).unwrap();
        symlink("missing", root.path().join("retired/dangling")).unwrap();
        let directory = Directory::open_host(root.path()).unwrap();
        directory.remove_tree(OsStr::new("retired")).unwrap();
        assert!(!root.path().join("retired").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("keep")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn enumerations_have_independent_offsets() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(root.path().join(name), name).unwrap();
        }
        let directory = Directory::open_host(root.path()).unwrap();
        let mut first = directory.entries().unwrap();
        let mut seen = vec![first.next().unwrap().unwrap()];
        let mut all = directory.names().unwrap();
        all.sort();
        assert_eq!(all, ["a", "b", "c"]);
        seen.extend(first.map(Result::unwrap));
        seen.sort();
        assert_eq!(seen, ["a", "b", "c"]);
    }

    #[test]
    fn rename_modes_refuse_replace_and_exchange() {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::open_host(root.path()).unwrap();
        std::fs::write(root.path().join("a"), "a").unwrap();
        std::fs::write(root.path().join("b"), "b").unwrap();
        let error = directory
            .rename(
                OsStr::new("a"),
                &directory,
                OsStr::new("b"),
                RenameMode::NoReplace,
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        directory
            .rename(
                OsStr::new("a"),
                &directory,
                OsStr::new("b"),
                RenameMode::Exchange,
            )
            .unwrap();
        assert_eq!(std::fs::read(root.path().join("a")).unwrap(), b"b");
        assert_eq!(std::fs::read(root.path().join("b")).unwrap(), b"a");
    }

    #[test]
    fn versions_change_with_content_identity() {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::open_host(root.path()).unwrap();
        std::fs::write(root.path().join("file"), "one").unwrap();
        let before = directory.stat(OsStr::new("file")).unwrap();
        assert_eq!(
            before.version(),
            directory.stat(OsStr::new("file")).unwrap().version()
        );
        std::fs::write(root.path().join("staged"), "two!").unwrap();
        std::fs::rename(root.path().join("staged"), root.path().join("file")).unwrap();
        assert_ne!(
            before.version(),
            directory.stat(OsStr::new("file")).unwrap().version()
        );
    }
}
