//! Native, handle-relative Windows filesystem operations. Each untrusted name
//! is one component in NtCreateFile's RootDirectory namespace. Reparse points
//! are opened as objects and interpreted by confinement, never followed here.
//! Directory handles deny write/delete sharing and retain their ancestors, so
//! a directory cannot be renamed or converted to a junction during a lookup.

use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::{self, Read},
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, Prefix},
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
};

use environment_protocol::data::fs::{FileKind, FileVersion};
use windows_sys::{
    Wdk::{Foundation::OBJECT_ATTRIBUTES, Storage::FileSystem as nt},
    Win32::{
        Foundation::{self as foundation, HANDLE, UNICODE_STRING},
        Storage::FileSystem as fs,
        System::IO::{DeviceIoControl, IO_STATUS_BLOCK},
    },
};

pub(crate) const SELF: &str = ".";
pub(crate) const DEFAULT_FILE_MODE: u32 = 0;
pub(crate) const DEFAULT_DIRECTORY_MODE: u32 = 0;
pub(crate) const PRIVATE_FILE_MODE: u32 = 0;
// An internal creation policy, not a Unix permission mapping.
pub(crate) const PRIVATE_DIRECTORY_MODE: u32 = u32::MAX;
const MAX_REMOVE_DEPTH: usize = 1024;
const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00a8;

#[derive(Clone, Debug)]
pub(crate) struct Directory {
    file: Arc<File>,
    // Ancestors stay pinned even after the resolver's temporary stack is gone.
    _parent: Option<Arc<Directory>>,
    enumeration: Arc<Mutex<()>>,
}

fn handle(file: &File) -> HANDLE {
    file.as_raw_handle()
}

fn nt_result(status: i32) -> io::Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        // SAFETY: converting an NTSTATUS has no pointer or ownership arguments.
        Err(io::Error::from_raw_os_error(
            unsafe { foundation::RtlNtStatusToDosError(status) } as i32,
        ))
    }
}

fn reparse_error() -> io::Error {
    io::Error::from_raw_os_error(foundation::ERROR_CANT_ACCESS_FILE as i32)
}

fn check_name(name: &OsStr) -> io::Result<Vec<u16>> {
    let name: Vec<u16> = name.encode_wide().collect();
    if name.is_empty()
        || name.len() > 32767
        || name.iter().any(|c| matches!(*c, 0 | 47 | 92 | 58))
        || name == [46]
        || name == [46, 46]
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one native entry name",
        ));
    }
    Ok(name)
}

impl Directory {
    pub fn open_host(path: &Path) -> io::Result<Self> {
        let path = crate::windows_security::local_drive_path(path)?;
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected drive root",
            ));
        };
        let Prefix::Disk(drive) = prefix.kind() else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "only local drives are supported",
            ));
        };
        if components.next() != Some(Component::RootDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected absolute drive path",
            ));
        }
        let root = format!("{}:\\", char::from(drive));
        let file = OpenOptions::new()
            .access_mode(fs::FILE_LIST_DIRECTORY | fs::FILE_READ_ATTRIBUTES | fs::SYNCHRONIZE)
            .share_mode(fs::FILE_SHARE_READ)
            .custom_flags(fs::FILE_FLAG_BACKUP_SEMANTICS | fs::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(root)?;
        let mut directory = Self::from_file(file, None)?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected normalized directory path",
                ));
            };
            directory = directory.child(name)?;
        }
        Ok(directory)
    }

    fn from_file(file: File, parent: Option<Arc<Directory>>) -> io::Result<Self> {
        let stat = EntryStat::of_file(&file)?;
        if stat.kind == FileKind::Symlink {
            return Err(reparse_error());
        }
        if !stat.is_dir() {
            return Err(io::Error::from_raw_os_error(
                foundation::ERROR_DIRECTORY as i32,
            ));
        }
        Ok(Self {
            file: Arc::new(file),
            _parent: parent,
            enumeration: Arc::new(Mutex::new(())),
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }

    fn open_relative(
        &self,
        name: &OsStr,
        access: u32,
        disposition: u32,
        options: u32,
        share: u32,
    ) -> io::Result<File> {
        let mut name = check_name(name)?;
        let mut unicode = UNICODE_STRING {
            Length: (name.len() * 2) as u16,
            MaximumLength: (name.len() * 2) as u16,
            Buffer: name.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: handle(&self.file),
            ObjectName: &mut unicode,
            Attributes: foundation::OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: null_mut(),
            SecurityQualityOfService: null_mut(),
        };
        let mut result = null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: all pointers reference live initialized storage for the call;
        // the successful returned handle is transferred exactly once to File.
        nt_result(unsafe {
            nt::NtCreateFile(
                &mut result,
                access | fs::SYNCHRONIZE,
                &attributes,
                &mut status,
                null(),
                fs::FILE_ATTRIBUTE_NORMAL,
                share,
                disposition,
                options | nt::FILE_OPEN_REPARSE_POINT | nt::FILE_SYNCHRONOUS_IO_NONALERT,
                null(),
                0,
            )
        })?;
        Ok(unsafe { File::from_raw_handle(result) })
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        if name == SELF {
            return self.try_clone();
        }
        let file = self.open_relative(
            name,
            fs::FILE_LIST_DIRECTORY | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            nt::FILE_DIRECTORY_FILE,
            fs::FILE_SHARE_READ,
        )?;
        Self::from_file(file, Some(Arc::new(self.clone())))
    }

    fn open_attributes(&self, name: &OsStr) -> io::Result<File> {
        if name == SELF {
            return self.file.try_clone();
        }
        self.open_relative(
            name,
            fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            0,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
        )
    }

    pub fn stat(&self, name: &OsStr) -> io::Result<EntryStat> {
        EntryStat::of_file(&self.open_attributes(name)?)
    }
    pub fn stat_with_birth(&self, name: &OsStr) -> io::Result<EntryStat> {
        self.stat(name)
    }

    pub fn read_link(&self, name: &OsStr) -> io::Result<OsString> {
        let file = self.open_attributes(name)?;
        let mut buffer = [0u8; 16 * 1024];
        let mut written = 0;
        // SAFETY: output buffer is writable and its size is passed exactly.
        if unsafe {
            DeviceIoControl(
                handle(&file),
                FSCTL_GET_REPARSE_POINT,
                null(),
                0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut written,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        decode_link(&buffer[..written as usize])
    }

    pub fn open_read(&self, name: &OsStr) -> io::Result<File> {
        let file = self.open_relative(
            name,
            fs::FILE_READ_DATA | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            nt::FILE_NON_DIRECTORY_FILE,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
        )?;
        if EntryStat::of_file(&file)?.kind == FileKind::Symlink {
            return Err(reparse_error());
        }
        Ok(file)
    }

    pub fn create_file(&self, name: &OsStr, _mode: u32) -> io::Result<File> {
        self.open_relative(
            name,
            fs::FILE_READ_DATA
                | fs::FILE_WRITE_DATA
                | fs::FILE_READ_ATTRIBUTES
                | fs::READ_CONTROL
                | fs::WRITE_DAC,
            nt::FILE_CREATE,
            nt::FILE_NON_DIRECTORY_FILE,
            // Deny readers/writers before the caller can replace the inherited
            // ACL. An empty staging file must not yield a readable handle that
            // would survive the later private-DACL update and observe content.
            fs::FILE_SHARE_DELETE,
        )
    }

    pub fn permissions(
        &self,
        name: &OsStr,
    ) -> io::Result<crate::windows_security::FilePermissions> {
        let file = self.open_relative(
            name,
            fs::READ_CONTROL | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            nt::FILE_NON_DIRECTORY_FILE,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
        )?;
        if !EntryStat::of_file(&file)?.is_file() {
            return Err(reparse_error());
        }
        crate::windows_security::FilePermissions::capture(&file)
    }

    pub fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        if mode == PRIVATE_DIRECTORY_MODE {
            return crate::windows_security::create_private_directory_at(&self.file, name)
                .map(drop);
        }
        self.open_relative(
            name,
            fs::FILE_LIST_DIRECTORY | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_CREATE,
            nt::FILE_DIRECTORY_FILE,
            fs::FILE_SHARE_READ,
        )
        .map(drop)
    }

    pub fn open_publish_file(&self, name: &OsStr) -> io::Result<File> {
        let file = self.open_relative(
            name,
            fs::FILE_READ_DATA
                | fs::FILE_WRITE_DATA
                | fs::FILE_READ_ATTRIBUTES
                | fs::READ_CONTROL
                | fs::WRITE_DAC,
            nt::FILE_OPEN,
            nt::FILE_NON_DIRECTORY_FILE,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
        )?;
        if !EntryStat::of_file(&file)?.is_file() {
            return Err(reparse_error());
        }
        Ok(file)
    }

    fn delete(&self, name: &OsStr, directory: bool) -> io::Result<()> {
        let file = self.open_relative(
            name,
            fs::DELETE | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            if directory {
                nt::FILE_DIRECTORY_FILE
            } else {
                0
            },
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
        )?;
        if !directory && EntryStat::of_file(&file)?.kind == FileKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "cannot unlink a directory",
            ));
        }
        delete_file(&file)
    }
    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        self.delete(name, false)
    }
    pub fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        self.delete(name, true)
    }

    pub fn rename(
        &self,
        from: &OsStr,
        destination: &Directory,
        to: &OsStr,
        mode: RenameMode,
    ) -> io::Result<()> {
        if mode == RenameMode::Exchange {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "atomic exchange is unavailable on Windows",
            ));
        }
        if EntryStat::of_directory(self)?.identity()
            != EntryStat::of_directory(destination)?.identity()
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cross-directory rename is unavailable with Windows directory confinement",
            ));
        }
        let name = check_name(to)?;
        let source = self.open_relative(
            from,
            fs::DELETE | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            0,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
        )?;
        // The native API requires at least the complete fixed-size structure,
        // including its alignment padding, even for one-character names.
        let bytes = size_of::<nt::FILE_RENAME_INFORMATION>() + name.len() * 2;
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let info = storage.as_mut_ptr().cast::<nt::FILE_RENAME_INFORMATION>();
        // SAFETY: usize storage gives the ABI alignment; variable-length name
        // occupies the allocated tail. Destination is a held directory handle.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = mode == RenameMode::Replace;
            // A same-directory rename takes a single name and null root. A
            // nonnull root makes Windows reopen the directory for write access,
            // conflicting with the pin that prevents junction replacement.
            (*info).RootDirectory = null_mut();
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        }
        let mut status = IO_STATUS_BLOCK::default();
        nt_result(unsafe {
            nt::NtSetInformationFile(
                handle(&source),
                &mut status,
                info.cast(),
                bytes as u32,
                nt::FileRenameInformation,
            )
        })
    }

    pub fn names(&self) -> io::Result<Vec<OsString>> {
        let _guard = self
            .enumeration
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut names = Vec::new();
        let mut buffer = vec![0u64; 8192];
        let mut restart = true;
        loop {
            let mut status = IO_STATUS_BLOCK::default();
            // SAFETY: aligned buffer is live through the synchronous NT query.
            let code = unsafe {
                nt::NtQueryDirectoryFile(
                    handle(&self.file),
                    null_mut(),
                    None,
                    null(),
                    &mut status,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 8) as u32,
                    nt::FileNamesInformation,
                    false,
                    null(),
                    restart,
                )
            };
            if code == foundation::STATUS_NO_MORE_FILES {
                break;
            }
            nt_result(code)?;
            restart = false;
            let used = status.Information;
            let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), used) };
            let mut offset = 0;
            loop {
                let entry = bytes
                    .get(offset..)
                    .ok_or_else(|| io::Error::other("invalid directory response"))?;
                if entry.len() < 12 {
                    return Err(io::Error::other("short directory response"));
                }
                let next = u32::from_ne_bytes(entry[0..4].try_into().unwrap()) as usize;
                let len = u32::from_ne_bytes(entry[8..12].try_into().unwrap()) as usize;
                let raw = entry
                    .get(12..12 + len)
                    .filter(|_| len.is_multiple_of(2))
                    .ok_or_else(|| io::Error::other("invalid directory name"))?;
                let wide: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_ne_bytes([b[0], b[1]]))
                    .collect();
                let name = OsString::from_wide(&wide);
                if name != "." && name != ".." {
                    names.push(name);
                }
                if next == 0 {
                    break;
                }
                if next < 12 {
                    return Err(io::Error::other("invalid directory offset"));
                }
                offset += next;
            }
        }
        Ok(names)
    }

    /// Windows has no unprivileged directory fsync API. Core writes flush their
    /// staged file before an atomic NT rename. Transfer journals recover daemon
    /// restarts; publication does not promise directory durability on power loss.
    pub fn sync(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn remove_tree(&self, name: &OsStr) -> io::Result<()> {
        self.remove_tree_at(name, 0)
    }
    fn remove_tree_at(&self, name: &OsStr, depth: usize) -> io::Result<()> {
        if depth >= MAX_REMOVE_DEPTH {
            return Err(io::Error::other("directory tree is too deep to remove"));
        }
        let file = self.open_relative(
            name,
            fs::DELETE | fs::FILE_LIST_DIRECTORY | fs::FILE_READ_ATTRIBUTES,
            nt::FILE_OPEN,
            0,
            fs::FILE_SHARE_READ,
        )?;
        let stat = EntryStat::of_file(&file)?;
        if !stat.is_dir() {
            return delete_file(&file);
        }
        let directory = Self::from_file(file, Some(Arc::new(self.clone())))?;
        for child in directory.names()? {
            match directory.remove_tree_at(&child, depth + 1) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
        delete_file(&directory.file)
    }
}

fn delete_file(file: &File) -> io::Result<()> {
    let info = nt::FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: fixed-size initialized information for a synchronous call.
    nt_result(unsafe {
        nt::NtSetInformationFile(
            handle(file),
            &mut status,
            (&info as *const nt::FILE_DISPOSITION_INFORMATION).cast(),
            size_of::<nt::FILE_DISPOSITION_INFORMATION>() as u32,
            nt::FileDispositionInformation,
        )
    })
}

fn decode_link(buffer: &[u8]) -> io::Result<OsString> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported or malformed reparse point",
        )
    };
    let read16 = |offset: usize| -> io::Result<u16> {
        Ok(u16::from_le_bytes(
            buffer
                .get(offset..offset + 2)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ))
    };
    let tag = u32::from_le_bytes(buffer.get(..4).ok_or_else(invalid)?.try_into().unwrap());
    let base = match tag {
        0xa000_000c => 20,
        0xa000_0003 => 16,
        _ => return Err(invalid()),
    };
    let offset = read16(8)? as usize;
    let len = read16(10)? as usize;
    if !len.is_multiple_of(2) || !offset.is_multiple_of(2) {
        return Err(invalid());
    }
    let raw = buffer
        .get(base + offset..base + offset + len)
        .ok_or_else(invalid)?;
    let wide: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let target = OsString::from_wide(&wide);
    let text = target.to_str().ok_or_else(invalid)?;
    // These prefixes come only from the kernel reparse record, never user paths.
    let kernel_absolute = text.starts_with(r"\??\");
    let text = text.strip_prefix(r"\??\").unwrap_or(text);
    let drive_absolute = text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && text.as_bytes().get(1) == Some(&b':')
        && matches!(text.as_bytes().get(2), Some(b'/' | b'\\'));
    if text.starts_with(['\\', '/'])
        || text.contains('\0')
        || ((kernel_absolute || text.contains(':')) && !drive_absolute)
    {
        return Err(invalid());
    }
    Ok(OsString::from(text))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameMode {
    Replace,
    NoReplace,
    Exchange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryStat {
    pub kind: FileKind,
    pub mode: u32,
    pub size: u64,
    pub dev: u64,
    pub ino: u128,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
    pub birth_ms: Option<i64>,
}

fn file_info<T: Default>(file: &File, class: fs::FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: callers pair a Windows FILE_INFO class with its documented struct.
    if unsafe {
        fs::GetFileInformationByHandleEx(
            handle(file),
            class,
            (&mut value as *mut T).cast(),
            size_of::<T>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

fn unix_time(ticks: i64) -> (i64, i64) {
    let ticks = ticks.saturating_sub(116_444_736_000_000_000);
    (
        ticks.div_euclid(10_000_000),
        ticks.rem_euclid(10_000_000) * 100,
    )
}
fn to_ms((secs, nanos): (i64, i64)) -> i64 {
    secs.saturating_mul(1000).saturating_add(nanos / 1_000_000)
}

impl EntryStat {
    pub fn of_file(file: &File) -> io::Result<Self> {
        let basic: fs::FILE_BASIC_INFO = file_info(file, fs::FileBasicInfo)?;
        let standard: fs::FILE_STANDARD_INFO = file_info(file, fs::FileStandardInfo)?;
        let identity: fs::FILE_ID_INFO = file_info(file, fs::FileIdInfo)?;
        let kind = if basic.FileAttributes & fs::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            FileKind::Symlink
        } else if standard.Directory {
            FileKind::Directory
        } else {
            FileKind::File
        };
        Ok(Self {
            kind,
            mode: 0,
            size: standard.EndOfFile.max(0) as u64,
            dev: identity.VolumeSerialNumber,
            ino: u128::from_le_bytes(identity.FileId.Identifier),
            modified: unix_time(basic.LastWriteTime),
            changed: unix_time(basic.ChangeTime),
            birth_ms: Some(to_ms(unix_time(basic.CreationTime))),
        })
    }
    pub fn of_directory(directory: &Directory) -> io::Result<Self> {
        Self::of_file(&directory.file)
    }
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory
    }
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }
    pub fn executable(&self) -> bool {
        false
    }
    pub fn modified_ms(&self) -> i64 {
        to_ms(self.modified)
    }
    pub fn identity(&self) -> (u64, u128) {
        (self.dev, self.ino)
    }
    pub fn unchanged(&self, other: &Self) -> bool {
        self.identity() == other.identity()
            && self.size == other.size
            && self.modified == other.modified
            && self.changed == other.changed
    }
    pub fn version(&self) -> FileVersion {
        FileVersion::new(format!(
            "win1-{:x}-{:x}-{:x}-{:x}.{:x}-{:x}.{:x}",
            self.dev,
            self.ino,
            self.size,
            self.modified.0 as u64,
            self.modified.1,
            self.changed.0 as u64,
            self.changed.1
        ))
        .expect("short printable file version")
    }
}

pub(crate) fn read_up_to(file: &mut File, max: u64) -> io::Result<Vec<u8>> {
    let mut data = Vec::with_capacity(max.min(1024 * 1024) as usize);
    file.take(max).read_to_end(&mut data)?;
    Ok(data)
}
pub(crate) fn utf8_name(name: &OsStr) -> Option<&str> {
    let name = name.to_str()?;
    environment_protocol::paths::EnvironmentPath::new(format!("C:/{name}")).ok()?;
    Some(name)
}
pub(crate) fn set_mode(_file: &File, mode: u32) -> io::Result<()> {
    if mode != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unix modes are unavailable on Windows",
        ));
    }
    Ok(())
}
pub(crate) fn with_execute_bits(mode: u32) -> u32 {
    mode | 0o111
}
