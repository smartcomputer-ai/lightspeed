//! What the file operations refuse besides the daemon state directory.
//!
//! The filesystem root bounds where a path may point, and it is `/` unless
//! configured, so two kinds of location inside it carry a rule of their own:
//!
//! - Kernel filesystems (`/proc` and `/sys` on Linux) expose the memory,
//!   environment and open descriptors of every process the daemon's user
//!   owns, the daemon itself included. They are refused whole, by device
//!   id, so the same filesystem bound at a second path and a link into it
//!   are refused alike. A separate instance mounted somewhere else has a
//!   device of its own and is not recognized.
//! - The registration key admits new machines to a universe. The file its
//!   configured path names is refused, and so is the whole volume when the
//!   key sits in a directory mounted to hold it.
//!
//! The rules bind file operations only. A command the daemon starts reads
//! whatever its user may read.

use std::{
    io,
    path::{Component, Path, PathBuf},
};

use super::{confinement::Identity, os::EntryStat};

/// Kernel filesystems this host mounts, by their usual mount points.
#[cfg(target_os = "linux")]
const KERNEL_FILESYSTEMS: [&str; 2] = ["/proc", "/sys"];
#[cfg(not(target_os = "linux"))]
const KERNEL_FILESYSTEMS: [&str; 0] = [];

/// Locations no data-plane path addresses, under any filesystem root.
#[derive(Clone, Debug, Default)]
pub struct Protected {
    /// Device ids of the filesystems refused whole.
    devices: Vec<u64>,
    /// Host paths of the files refused one by one. Each is recognized by
    /// the file its path names when an operation starts, never by an inode
    /// recorded at startup: replacing the file moves the rule to the new
    /// one, and an inode the old file gave back refuses nothing.
    files: Vec<PathBuf>,
}

/// The protected files as they are when one operation starts.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProtectedFiles(Vec<Identity>);

impl Protected {
    /// What a serving daemon refuses on this host: the kernel filesystems
    /// mounted here, and the registration key when it was read from a file.
    /// `working` names the directories file operations exist to reach (the
    /// filesystem root and the working directory); a volume that holds one
    /// of them is never refused whole.
    pub fn for_daemon(registration_key_file: Option<&Path>, working: &[&Path]) -> Self {
        let mut protected = Self::default();
        for mount in KERNEL_FILESYSTEMS {
            // A host without the mount has an ordinary directory there, or
            // nothing: refusing its device would refuse the disk it is on.
            if let Some(device) = mounted_device(Path::new(mount)) {
                protected.refuse_device(device);
            }
        }
        if let Some(path) = registration_key_file {
            protected.refuse_key_file(path, working);
        }
        protected
    }

    /// Refuse every entry of the filesystem `path` is on.
    pub fn refuse_filesystem_at(&mut self, path: &Path) -> io::Result<()> {
        let (device, _) = directory_identity(path)?;
        self.refuse_device(device);
        Ok(())
    }

    /// Refuse the file `path` names, under every name that reaches it, and
    /// refuse removing, renaming, replacing or capturing a directory that
    /// holds it. Both the path as given and what it resolves to now are
    /// kept, so a link that is later pointed at a successor still names the
    /// protected file.
    pub fn refuse_file(&mut self, path: &Path) {
        let written = without_parent_steps(path);
        let target = resolved(&written);
        for path in [written, target] {
            if !self.files.contains(&path) {
                self.files.push(path);
            }
        }
    }

    /// Refuse the registration key read from `configured`: the file itself,
    /// and the whole volume when a directory that holds it is a mount of
    /// its own. A secret volume replaces its files, and the directory they
    /// sit in, whenever the secret rotates, so no path or inode recorded
    /// now would still name the key then; its device does.
    pub fn refuse_key_file(&mut self, configured: &Path, working: &[&Path]) {
        let kept: Vec<u64> = working
            .iter()
            .filter_map(|path| directory_identity(path).ok())
            .map(|(device, _)| device)
            .collect();
        let target = resolved(configured);
        for holder in [configured.parent(), target.parent()].into_iter().flatten() {
            if let Some(device) = mounted_device(holder)
                && !kept.contains(&device)
                && self.refuse_device(device)
            {
                // Said once at startup: a refusal anywhere on that volume
                // is otherwise hard to trace back to where the key is kept.
                eprintln!(
                    "lightspeed-envd: file operations refuse the whole volume mounted at {}: \
                     it holds the registration key file",
                    resolved(holder).display()
                );
            }
        }
        self.refuse_file(configured);
    }

    /// Whether the device was not refused before.
    fn refuse_device(&mut self, device: u64) -> bool {
        let added = !self.devices.contains(&device);
        if added {
            self.devices.push(device);
        }
        added
    }

    /// Identify the protected files as they are now.
    pub(crate) fn files_now(&self) -> ProtectedFiles {
        ProtectedFiles(
            self.files
                .iter()
                .filter_map(|path| file_identity(path))
                .collect(),
        )
    }

    /// The entry is on a refused filesystem or is one of `files`.
    pub(crate) fn covers(&self, files: &ProtectedFiles, stat: &EntryStat) -> bool {
        self.devices.contains(&stat.dev) || files.0.contains(&stat.identity())
    }

    /// The directory is, now, an ancestor of a protected file: removing,
    /// renaming, replacing or capturing it would address that file.
    pub(crate) fn holds_file(&self, directory: &EntryStat) -> bool {
        let identity = directory.identity();
        self.files.iter().any(|path| {
            path.ancestors()
                .skip(1)
                .any(|ancestor| directory_identity(ancestor).is_ok_and(|held| held == identity))
        })
    }
}

/// What `path` resolves to now, or `path` itself while nothing is there.
fn resolved(path: &Path) -> PathBuf {
    let Ok(target) = path.canonicalize() else {
        return path.to_path_buf();
    };
    #[cfg(windows)]
    let target = crate::windows_security::local_drive_path(&target).unwrap_or(target);
    target
}

/// `path` without `..` steps: its directory resolved, its last name kept as
/// written. Read as written, `mount/../key` has `mount` among its ancestors,
/// though it names nothing there. The last name stays unresolved because a
/// link in that position is how a rotated key is published.
fn without_parent_steps(path: &Path) -> PathBuf {
    if !path.components().any(|part| part == Component::ParentDir) {
        return path.to_path_buf();
    }
    match (path.parent(), path.file_name()) {
        (Some(directory), Some(name)) => resolved(directory).join(name),
        _ => path.to_path_buf(),
    }
}

/// The device of `directory` when it is a mount point: its device differs
/// from its parent's. The directory is resolved first, so the parent is the
/// directory that holds it, whatever link or `..` step the path was written
/// with.
fn mounted_device(directory: &Path) -> Option<u64> {
    let directory = resolved(directory);
    let (device, _) = directory_identity(&directory).ok()?;
    let (parent, _) = directory_identity(directory.parent()?).ok()?;
    (device != parent).then_some(device)
}

/// Device and inode of the directory at a host path.
fn directory_identity(path: &Path) -> io::Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)?;
        Ok((metadata.dev(), metadata.ino() as u128))
    }
    #[cfg(windows)]
    {
        let directory = super::os::Directory::open_host(path)?;
        Ok(EntryStat::of_directory(&directory)?.identity())
    }
}

/// Device and inode of the file a host path names now, if any.
fn file_identity(path: &Path) -> Option<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.dev(), metadata.ino() as u128))
    }
    #[cfg(windows)]
    {
        let directory = super::os::Directory::open_host(path.parent()?).ok()?;
        Some(directory.stat(path.file_name()?).ok()?.identity())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    fn device(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().dev()
    }

    /// `/dev` is a mount of its own on every Unix the daemon runs on, which
    /// makes it the one mount point a test can rely on without privileges.
    fn dev_is_mounted(elsewhere: &Path) {
        assert_ne!(
            device(Path::new("/dev")),
            device(elsewhere),
            "this test needs /dev to be a filesystem of its own"
        );
        assert_eq!(
            mounted_device(Path::new("/dev")),
            Some(device(Path::new("/dev")))
        );
    }

    #[test]
    fn only_a_mount_point_yields_a_device() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        std::fs::create_dir(base.join("plain")).unwrap();
        dev_is_mounted(&base);
        assert_eq!(mounted_device(&base.join("plain")), None);
        assert_eq!(mounted_device(&base.join("missing")), None);
        assert_eq!(mounted_device(Path::new("/")), None);
        // What holds `/dev/..` is not `/dev`, and a link to an ordinary
        // directory on another filesystem is not a mount point either.
        assert_eq!(mounted_device(Path::new("/dev/..")), None);
        std::os::unix::fs::symlink(base.join("plain"), base.join("link")).unwrap();
        assert_eq!(mounted_device(&base.join("link")), None);
        std::os::unix::fs::symlink("/dev", base.join("device-link")).unwrap();
        assert_eq!(
            mounted_device(&base.join("device-link")),
            Some(device(Path::new("/dev")))
        );
    }

    #[test]
    fn a_key_path_written_with_parent_steps_has_only_its_real_ancestors() {
        use std::ffi::OsStr;

        use crate::filesystem::os::Directory;

        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        std::fs::create_dir(base.join("work")).unwrap();
        std::fs::write(base.join("key"), "lsrk_secret").unwrap();
        let mut protected = Protected::default();
        protected.refuse_file(&base.join("work/../key"));
        assert_eq!(protected.files, [base.join("key")]);
        // `work` is beside the key, not around it: removing or capturing it
        // does not address the key.
        let directory = Directory::open_host(&base).unwrap();
        assert!(!protected.holds_file(&directory.stat(OsStr::new("work")).unwrap()));
        assert!(protected.holds_file(&EntryStat::of_directory(&directory).unwrap()));
    }

    #[test]
    fn a_key_in_an_ordinary_directory_protects_the_file_alone() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        std::fs::write(base.join("key"), "lsrk_secret").unwrap();
        let protected = Protected::for_daemon(Some(&base.join("key")), &[Path::new("/")]);
        // Nothing on this disk is refused whole; the kernel filesystems of
        // the host, where it has any, are a different device.
        assert!(!protected.devices.contains(&device(&base)));
        assert_eq!(protected.files, [base.join("key")]);
    }

    #[test]
    fn a_key_on_a_volume_of_its_own_protects_the_volume() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        dev_is_mounted(&base);
        let key = Path::new("/dev/null");
        let mut protected = Protected::default();
        protected.refuse_key_file(key, &[Path::new("/"), &base]);
        assert_eq!(protected.devices, [device(Path::new("/dev"))]);

        // The volume the agent works on stays reachable, key or no key.
        let mut shared = Protected::default();
        shared.refuse_key_file(key, &[Path::new("/dev")]);
        assert!(shared.devices.is_empty());
        assert_eq!(shared.files, [PathBuf::from("/dev/null")]);
    }

    #[test]
    fn a_key_behind_a_link_is_kept_by_both_paths() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        std::fs::create_dir(base.join("current")).unwrap();
        std::fs::write(base.join("current/key"), "lsrk_secret").unwrap();
        std::os::unix::fs::symlink("current/key", base.join("key")).unwrap();
        let mut protected = Protected::default();
        protected.refuse_file(&base.join("key"));
        assert_eq!(
            protected.files,
            [base.join("key"), base.join("current/key")]
        );

        let key = std::fs::metadata(base.join("current/key")).unwrap();
        let now = protected.files_now();
        assert!(now.0.contains(&(key.dev(), key.ino() as u128)));
        // Pointing the link at a successor moves the rule with it.
        std::fs::create_dir(base.join("next")).unwrap();
        std::fs::write(base.join("next/key"), "lsrk_rotated").unwrap();
        std::fs::remove_file(base.join("key")).unwrap();
        std::os::unix::fs::symlink("next/key", base.join("key")).unwrap();
        let successor = std::fs::metadata(base.join("next/key")).unwrap();
        assert!(
            protected
                .files_now()
                .0
                .contains(&(successor.dev(), successor.ino() as u128))
        );
    }
}
