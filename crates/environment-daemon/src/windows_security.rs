//! Native account directories and private daemon-state ACLs.
//!
//! Private objects grant access only to the execution account and SYSTEM.
//! The protected DACL is supplied at creation, before identity bytes are written;
//! existing objects are restricted through a validated handle without following
//! a final reparse point. These ACLs do not sandbox children running as that user.

use std::{
    ffi::{OsStr, OsString, c_void},
    fs::File,
    io,
    mem::{offset_of, size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr::{null, null_mut},
};

use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
            FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
            FileRenameInformation, NtCreateFile, NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_INSUFFICIENT_BUFFER, INVALID_HANDLE_VALUE, LocalFree, OBJ_CASE_INSENSITIVE,
            RtlNtStatusToDosError, UNICODE_STRING,
        },
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetSecurityDescriptorOwner, GetTokenInformation, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
            TokenUser, UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, GetFileInformationByHandle, OPEN_EXISTING,
            READ_CONTROL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER,
        },
        System::{
            Com::CoTaskMemFree,
            IO::IO_STATUS_BLOCK,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
        UI::Shell::{
            FOLDERID_LocalAppData, FOLDERID_Profile, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
        },
    },
    core::GUID,
};

/// Home of the account executing the daemon, even without a logged-in desktop.
pub(crate) fn user_home() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_Profile)
}

/// Local application data of the execution account, not an inherited HOME value.
pub(crate) fn local_app_data() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_LocalAppData)
}

fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    let mut value = null_mut();
    // SAFETY: the API returns an allocated, nul-terminated UTF-16 string. A null
    // token selects the current account. CoTaskMemFree owns the returned buffer.
    let status =
        unsafe { SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY as u32, null_mut(), &mut value) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "resolve account folder: HRESULT {status:#x}"
        )));
    }
    if value.is_null() {
        return Err(io::Error::other("account folder is missing"));
    }
    let path = unsafe {
        let mut len = 0;
        while *value.add(len) != 0 {
            len += 1;
        }
        let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(value, len)));
        CoTaskMemFree(value.cast());
        path
    };
    local_drive_path(&path)
}

/// Convert a trusted OS-resolved local drive path to ordinary native spelling.
/// This is for `canonicalize`/known-folder outputs, never caller namespace input.
/// UNC, volume GUID and device roots remain unsupported.
pub(crate) fn local_drive_path(path: &Path) -> io::Result<PathBuf> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(unsupported_path());
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
        _ => return Err(unsupported_path()),
    };
    if components.next() != Some(Component::RootDir) {
        return Err(unsupported_path());
    }
    let mut result = PathBuf::from(format!("{}:\\", char::from(drive.to_ascii_uppercase())));
    for component in components {
        match component {
            Component::Normal(name) => result.push(name),
            _ => return Err(unsupported_path()),
        }
    }
    Ok(result)
}

fn unsupported_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "daemon state and home require a local drive-absolute path",
    )
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<_> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: every instance comes from an API documented to use LocalAlloc.
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct PrivateDescriptor(LocalAllocation);

impl PrivateDescriptor {
    fn new(directory: bool) -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: valid pseudo-process handle; the returned token is owned.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut bytes = 0;
        unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes);
        }
        if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(io::Error::last_os_error());
        }
        // usize elements provide alignment for TOKEN_USER and the trailing SID.
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_text = null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid_allocation = LocalAllocation(sid_text.cast());
        let sid = unsafe {
            let mut len = 0;
            while *sid_text.add(len) != 0 {
                len += 1;
            }
            String::from_utf16(std::slice::from_raw_parts(sid_text, len))
                .map_err(io::Error::other)?
        };
        drop(sid_allocation);
        let inheritance = if directory { "OICI" } else { "" };
        let sddl = format!("O:{sid}D:P(A;{inheritance};FA;;;{sid})(A;{inheritance};FA;;;SY)");
        let sddl = wide(OsStr::new(&sddl))?;
        let mut descriptor = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(LocalAllocation(descriptor)))
    }

    fn restrict(&self, file: &File) -> io::Result<()> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = null_mut();
        if unsafe { GetSecurityDescriptorDacl(self.0.0, &mut present, &mut dacl, &mut defaulted) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::other("private descriptor has no DACL"));
        }
        let mut owner = null_mut();
        if unsafe { GetSecurityDescriptorOwner(self.0.0, &mut owner, &mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let error = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                null_mut(),
                dacl,
                null(),
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(())
    }
}

fn validate_object(file: &File, directory: bool) -> io::Result<()> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state may not be a reparse point",
        ));
    }
    if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state object has the wrong type",
        ));
    }
    if !directory && info.nNumberOfLinks != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private material may not have hard-link aliases",
        ));
    }
    Ok(())
}

/// Open one component relative to a pinned directory. All object opens suppress
/// reparse traversal; no directory permits write or delete sharing, preventing
/// junction conversion and rename while descendants are being resolved.
fn relative_open(
    parent: &File,
    name: &OsStr,
    directory: bool,
    create: u32,
    access: u32,
    descriptor: Option<&PrivateDescriptor>,
) -> io::Result<File> {
    let mut encoded = wide(name)?;
    if encoded.len() > 32767
        || encoded[..encoded.len() - 1]
            .iter()
            .any(|unit| matches!(*unit, 47 | 92 | 58))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private object requires a single ordinary name",
        ));
    }
    let mut name = UNICODE_STRING {
        Length: ((encoded.len() - 1) * 2) as u16,
        MaximumLength: (encoded.len() * 2) as u16,
        Buffer: encoded.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &mut name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: descriptor.map_or(null(), |value| value.0.0.cast()),
        SecurityQualityOfService: null_mut(),
    };
    let mut handle = null_mut();
    let mut status: IO_STATUS_BLOCK = unsafe { zeroed() };
    let options = if directory {
        FILE_DIRECTORY_FILE
    } else {
        FILE_NON_DIRECTORY_FILE
    };
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            &attributes,
            &mut status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | if directory { 0 } else { FILE_SHARE_DELETE },
            create,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ));
    }
    let file = unsafe { File::from_raw_handle(handle) };
    validate_object(&file, directory)?;
    Ok(file)
}

struct PinnedDirectory {
    handles: Vec<File>,
}

impl PinnedDirectory {
    fn open(path: &Path, create: bool, private_leaf: bool) -> io::Result<Self> {
        let path = local_drive_path(path)?;
        let mut components = path.components();
        let root: PathBuf = components.by_ref().take(2).collect();
        let names: Vec<_> = components.map(|component| component.as_os_str()).collect();
        if private_leaf && names.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a drive root cannot be daemon state",
            ));
        }
        let root = wide(root.as_os_str())?;
        let handle = unsafe {
            CreateFileW(
                root.as_ptr(),
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
                FILE_SHARE_READ,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let root = unsafe { File::from_raw_handle(handle) };
        validate_object(&root, true)?;
        let mut pinned = Self {
            handles: vec![root],
        };
        for (index, name) in names.iter().enumerate() {
            let final_private = private_leaf && index + 1 == names.len();
            let descriptor = final_private
                .then(|| PrivateDescriptor::new(true))
                .transpose()?;
            let access = FILE_LIST_DIRECTORY
                | if final_private {
                    READ_CONTROL | WRITE_DAC | WRITE_OWNER
                } else {
                    0
                };
            let child = relative_open(
                pinned.leaf(),
                name,
                true,
                if create { FILE_OPEN_IF } else { FILE_OPEN },
                access,
                descriptor.as_ref(),
            )?;
            if let Some(descriptor) = descriptor {
                descriptor.restrict(&child)?;
            }
            pinned.handles.push(child);
        }
        Ok(pinned)
    }

    fn leaf(&self) -> &File {
        self.handles.last().expect("pinned root")
    }
}

/// Create private state atomically while keeping every ancestor pinned; reject
/// reparse points at every level, including existing operator-configured paths.
pub(crate) fn prepare_private_directory(path: &Path) -> io::Result<()> {
    PinnedDirectory::open(path, true, true).map(|_| ())
}

fn file_parent(path: &Path) -> io::Result<(PinnedDirectory, &OsStr)> {
    let parent = path.parent().ok_or_else(unsupported_path)?;
    let name = path.file_name().ok_or_else(unsupported_path)?;
    Ok((PinnedDirectory::open(parent, false, false)?, name))
}

/// Create a new private file atomically; its handle is not inherited by children.
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    let (parent, name) = file_parent(path)?;
    relative_open(
        parent.leaf(),
        name,
        false,
        FILE_CREATE,
        windows_sys::Win32::Foundation::GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
        Some(&PrivateDescriptor::new(false)?),
    )
}

/// Open material through pinned ancestors, reject hard links and final reparse
/// points, and apply the account's protected DACL before reading any bytes.
pub(crate) fn open_private_file(path: &Path) -> io::Result<File> {
    let (parent, name) = file_parent(path)?;
    let file = relative_open(
        parent.leaf(),
        name,
        false,
        FILE_OPEN,
        windows_sys::Win32::Foundation::GENERIC_READ | READ_CONTROL | WRITE_DAC | WRITE_OWNER,
        None,
    )?;
    PrivateDescriptor::new(false)?.restrict(&file)?;
    Ok(file)
}

/// Atomic no-replace install without a temporary hard-link alias. Both paths
/// must have the same parent; concurrent first starts keep the first key.
pub(crate) fn install_private_file(source: &Path, destination: &Path) -> io::Result<()> {
    if source.parent() != destination.parent() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity install must stay in its state directory",
        ));
    }
    let (parent, source_name) = file_parent(source)?;
    let file = relative_open(parent.leaf(), source_name, false, FILE_OPEN, DELETE, None)?;
    let name = destination.file_name().ok_or_else(unsupported_path)?;
    let encoded: Vec<_> = name.encode_wide().collect();
    let bytes = offset_of!(FILE_RENAME_INFORMATION, FileName) + encoded.len() * 2;
    let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let rename = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut status: IO_STATUS_BLOCK = unsafe { zeroed() };
    unsafe {
        (*rename).Anonymous.ReplaceIfExists = false;
        // A null root and a single filename rename in the source directory.
        // The source was opened relative to our still-pinned parent chain.
        (*rename).RootDirectory = null_mut();
        (*rename).FileNameLength = (encoded.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            encoded.as_ptr(),
            (*rename).FileName.as_mut_ptr(),
            encoded.len(),
        );
        let result = NtSetInformationFile(
            file.as_raw_handle(),
            &mut status,
            rename.cast(),
            bytes as u32,
            FileRenameInformation,
        );
        if result < 0 {
            return Err(io::Error::from_raw_os_error(
                RtlNtStatusToDosError(result) as i32
            ));
        }
    }
    Ok(())
}

/// An existing file's DACL and inheritance policy, held independently of paths.
pub(crate) struct FilePermissions {
    descriptor: LocalAllocation,
    protected: bool,
}

impl FilePermissions {
    pub(crate) fn capture(file: &File) -> io::Result<Self> {
        let mut descriptor = null_mut();
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let descriptor = LocalAllocation(descriptor);
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            protected: control & SE_DACL_PROTECTED != 0,
        })
    }

    pub(crate) fn apply(&self, file: &File) -> io::Result<()> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = null_mut();
        if unsafe {
            GetSecurityDescriptorDacl(self.descriptor.0, &mut present, &mut dacl, &mut defaulted)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot preserve an absent or null file DACL",
            ));
        }
        let mode = if self.protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        let error = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | mode,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(())
    }
}

/// Restrict an empty staging file before writing content; callers may restore a
/// captured destination DACL just before atomic publication.
pub(crate) fn restrict_private_file(file: &File) -> io::Result<()> {
    FilePermissions {
        descriptor: PrivateDescriptor::new(false)?.0,
        protected: true,
    }
    .apply(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;

    fn descriptor_text(descriptor: *mut c_void) -> String {
        let mut text = null_mut();
        assert_ne!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    null_mut(),
                )
            },
            0
        );
        let allocation = LocalAllocation(text.cast());
        let value = unsafe {
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            String::from_utf16(std::slice::from_raw_parts(text, length)).unwrap()
        };
        drop(allocation);
        value
    }

    fn assert_private(file: &File, directory: bool) {
        let mut descriptor = null_mut();
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        let descriptor = LocalAllocation(descriptor);
        let actual = descriptor_text(descriptor.0);
        let expected = PrivateDescriptor::new(directory).unwrap();
        let expected = descriptor_text(expected.0.0);
        assert!(
            actual.starts_with("D:P"),
            "DACL must be protected: {actual}"
        );
        // Windows may additionally mark the ACL auto-inherited; that control
        // flag does not add trustees or permissions to the protected ACE list.
        assert_eq!(
            &actual[actual.find('(').unwrap()..],
            &expected[expected.find('(').unwrap()..],
            "private ACL contains only the execution account and SYSTEM"
        );
    }

    #[test]
    fn account_folders_are_native_local_absolute_paths() {
        for directory in [user_home().unwrap(), local_app_data().unwrap()] {
            assert!(directory.is_absolute());
            assert!(
                matches!(directory.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)))
            );
        }
    }

    #[test]
    fn canonical_local_prefixes_are_converted_without_accepting_other_namespaces() {
        assert_eq!(
            local_drive_path(Path::new(r"\\?\c:\work\雪")).unwrap(),
            PathBuf::from(r"C:\work\雪")
        );
        assert_eq!(
            local_drive_path(Path::new(r"D:\")).unwrap(),
            PathBuf::from(r"D:\")
        );
        for path in [
            r"\\server\share",
            r"\\?\UNC\server\share",
            r"\\.\PhysicalDrive0",
            r"C:relative",
            r"\rooted",
        ] {
            assert_eq!(
                local_drive_path(Path::new(path)).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
    }

    #[test]
    fn private_creation_and_restriction_use_protected_account_acls() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        prepare_private_directory(&state).unwrap();
        // A directory requires BACKUP_SEMANTICS, but no write privileges to inspect.
        let state_wide = wide(state.as_os_str()).unwrap();
        let raw = unsafe {
            CreateFileW(
                state_wide.as_ptr(),
                READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        };
        assert_ne!(raw, INVALID_HANDLE_VALUE);
        let directory = unsafe { File::from_raw_handle(raw) };
        assert_private(&directory, true);
        drop(directory);
        let path = state.join("key");
        let mut file = create_private_file(&path).unwrap();
        assert_private(&file, false);
        file.write_all(b"private material").unwrap();
        drop(file);
        let mut file = open_private_file(&path).unwrap();
        assert_private(&file, false);
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "private material");
        drop(file);
        assert_eq!(
            create_private_file(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(prepare_private_directory(&path).is_err());
    }

    #[test]
    fn private_identity_reads_reject_reparse_points() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::write(&target, "outside").unwrap();
        let link = temp.path().join("link");
        std::os::windows::fs::symlink_file(&target, &link)
            .expect("this Windows filesystem test requires symbolic-link creation privilege or Developer Mode");
        assert_eq!(
            open_private_file(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "outside");
    }

    #[test]
    fn existing_identity_acl_is_restricted_and_state_links_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy-key");
        std::fs::write(&path, "legacy").unwrap();
        let file = open_private_file(&path).unwrap();
        assert_private(&file, false);
        drop(file);
        let target = temp.path().join("outside");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("state-link");
        std::os::windows::fs::symlink_dir(&target, &link)
            .expect("this Windows filesystem test requires symbolic-link creation privilege or Developer Mode");
        assert_eq!(
            prepare_private_directory(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(target.is_dir());
    }

    #[test]
    fn ancestor_handles_prevent_rename_and_reparse_parents_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let state = parent.join("state");
        prepare_private_directory(&state).unwrap();
        let pinned = PinnedDirectory::open(&state, false, false).unwrap();
        assert!(std::fs::rename(&parent, temp.path().join("moved")).is_err());
        assert!(std::fs::rename(&state, parent.join("moved-state")).is_err());
        drop(pinned);
        std::fs::rename(&parent, temp.path().join("moved")).unwrap();
        std::os::windows::fs::symlink_dir(temp.path().join("moved"), &parent)
            .expect("this test requires symbolic-link creation privilege or Developer Mode");
        assert_eq!(
            prepare_private_directory(&parent.join("new-state"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!temp.path().join("moved/new-state").exists());
    }

    #[test]
    fn identity_install_is_no_replace_and_hard_link_aliases_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let staged = temp.path().join("staged");
        let installed = temp.path().join("key");
        create_private_file(&staged)
            .unwrap()
            .write_all(b"first")
            .unwrap();
        install_private_file(&staged, &installed).unwrap();
        assert!(!staged.exists());
        create_private_file(&staged)
            .unwrap()
            .write_all(b"second")
            .unwrap();
        assert_eq!(
            install_private_file(&staged, &installed)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&installed).unwrap(), b"first");
        let alias = temp.path().join("alias");
        std::fs::hard_link(&installed, &alias).unwrap();
        assert_eq!(
            open_private_file(&installed).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            open_private_file(&alias).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn staging_can_restore_native_permissions_after_private_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = File::create(temp.path().join("source")).unwrap();
        let permissions = FilePermissions::capture(&source).unwrap();
        let expected = descriptor_text(permissions.descriptor.0);
        let mut destination = create_private_file(&temp.path().join("destination")).unwrap();
        restrict_private_file(&destination).unwrap();
        assert_private(&destination, false);
        destination.write_all(b"staged content").unwrap();
        permissions.apply(&destination).unwrap();
        let actual = FilePermissions::capture(&destination).unwrap();
        assert_eq!(descriptor_text(actual.descriptor.0), expected);
    }
}
