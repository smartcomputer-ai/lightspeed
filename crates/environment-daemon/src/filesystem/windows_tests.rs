use super::*;
use environment_protocol::{
    data::fs::{GlobFilesLimits, SearchTextLimits, TreeFilter},
    error::{ErrorData, ProtocolErrorKind},
    paths::TreePath,
};
use std::{
    ffi::OsStr,
    fs,
    os::windows::fs::{symlink_dir, symlink_file},
    sync::Arc,
};

struct Fixture {
    filesystem: LocalFileSystem,
    root: PathBuf,
    outside: PathBuf,
    _temp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let base =
        crate::windows_security::local_drive_path(&temp.path().canonicalize().unwrap()).unwrap();
    let root = base.join("work space 雪");
    let outside = base.join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let confinement = Arc::new(Confinement::new(root.clone(), root.join(".state")).unwrap());
    let filesystem = LocalFileSystem::new(FilesystemConfig {
        confinement,
        cwd: root.clone(),
        writable: true,
        limits: crate::data_limits().filesystem,
    })
    .unwrap();
    Fixture {
        filesystem,
        root,
        outside,
        _temp: temp,
    }
}

fn path(fixture: &Fixture, relative: &str) -> EnvironmentPath {
    fixture
        .filesystem
        .confinement()
        .root_path()
        .join(&TreePath::new(relative).unwrap())
        .unwrap()
}

fn reason(error: &ProtocolError) -> Option<PermissionReason> {
    match error.data {
        ErrorData::PermissionDenied { reason } => reason,
        _ => None,
    }
}

fn set_junction(path: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
        System::IO::DeviceIoControl,
    };
    let target = crate::windows_security::local_drive_path(target)?;
    let substitute: Vec<u16> = OsString::from(format!("\\??\\{}", target.display()))
        .encode_wide()
        .collect();
    let printed: Vec<u16> = target.as_os_str().encode_wide().collect();
    let mut bytes = Vec::new();
    bytes.extend(0xa000_0003u32.to_le_bytes());
    bytes.extend(((8 + (substitute.len() + printed.len() + 2) * 2) as u16).to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(((substitute.len() * 2) as u16).to_le_bytes());
    bytes.extend((((substitute.len() + 1) * 2) as u16).to_le_bytes());
    bytes.extend(((printed.len() * 2) as u16).to_le_bytes());
    for unit in substitute.into_iter().chain([0]).chain(printed).chain([0]) {
        bytes.extend(unit.to_le_bytes());
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut written = 0;
    // SAFETY: the test builds the documented mount-point reparse structure;
    // the handle and buffer remain alive through this synchronous call.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x0009_00a4,
            bytes.as_ptr().cast(),
            bytes.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut written,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[test]
fn native_junctions_obey_confinement_and_cannot_replace_held_directories() {
    let fixture = fixture();
    fs::create_dir(fixture.root.join("real")).unwrap();
    fs::write(fixture.root.join("real/file"), "inside").unwrap();
    fs::create_dir(fixture.root.join("junction")).unwrap();
    set_junction(&fixture.root.join("junction"), &fixture.root.join("real")).unwrap();
    assert_eq!(
        fixture
            .filesystem
            .confinement()
            .resolve(&path(&fixture, "junction/file"))
            .unwrap(),
        fixture.root.join("real/file")
    );
    fs::create_dir(fixture.root.join("escape-junction")).unwrap();
    set_junction(&fixture.root.join("escape-junction"), &fixture.outside).unwrap();
    assert_eq!(
        reason(
            &fixture
                .filesystem
                .confinement()
                .resolve(&path(&fixture, "escape-junction"))
                .unwrap_err()
        ),
        Some(PermissionReason::OutsideRoot)
    );
    fs::create_dir(fixture.root.join("held")).unwrap();
    let pinned = fixture
        .filesystem
        .confinement()
        .pin_directory(&path(&fixture, "held"))
        .unwrap();
    assert!(set_junction(pinned.path(), &fixture.outside).is_err());
}

#[test]
fn native_handles_publish_atomically_and_enumerate_without_shared_offsets() {
    let fixture = fixture();
    let directory = Directory::open_host(&fixture.root).unwrap();
    fs::write(fixture.root.join("before"), "old").unwrap();
    fs::write(fixture.root.join("staged"), "new").unwrap();
    let before = directory.stat(OsStr::new("before")).unwrap().version();
    assert_eq!(
        directory
            .rename(
                OsStr::new("staged"),
                &directory,
                OsStr::new("before"),
                RenameMode::NoReplace
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    directory
        .rename(
            OsStr::new("staged"),
            &directory,
            OsStr::new("before"),
            RenameMode::Replace,
        )
        .unwrap();
    assert_eq!(fs::read(fixture.root.join("before")).unwrap(), b"new");
    assert_ne!(
        before,
        directory.stat(OsStr::new("before")).unwrap().version()
    );
    let mut first = directory.names().unwrap();
    first.sort();
    let mut second = directory.names().unwrap();
    second.sort();
    assert_eq!(first, second);
    assert_eq!(first, [".state", "before"]);
}

#[test]
fn empty_staging_files_do_not_grant_read_handles_before_the_acl_is_restricted() {
    let fixture = fixture();
    let directory = Directory::open_host(&fixture.root).unwrap();
    let mut staged = directory.create_file(OsStr::new("stage"), 0).unwrap();
    assert_eq!(
        directory
            .open_read(OsStr::new("stage"))
            .unwrap_err()
            .raw_os_error(),
        Some(32)
    );
    crate::windows_security::restrict_private_file(&staged).unwrap();
    staged.write_all(b"private content").unwrap();
    directory
        .rename(
            OsStr::new("stage"),
            &directory,
            OsStr::new("published"),
            RenameMode::NoReplace,
        )
        .unwrap();
    drop(staged);
    assert_eq!(
        fs::read(fixture.root.join("published")).unwrap(),
        b"private content"
    );
}

#[test]
fn native_confinement_follows_inside_links_and_refuses_state_and_escape() {
    let fixture = fixture();
    fs::create_dir(fixture.root.join("real")).unwrap();
    fs::write(fixture.root.join("real/file"), "inside").unwrap();
    fs::write(fixture.outside.join("secret"), "outside").unwrap();
    symlink_dir("real", fixture.root.join("alias"))
        .expect("Windows developer machine permits symbolic links");
    symlink_dir(&fixture.outside, fixture.root.join("escape")).unwrap();
    symlink_dir(".state", fixture.root.join("state-alias")).unwrap();
    symlink_file("real/file", fixture.root.join("file-alias")).unwrap();
    let confinement = fixture.filesystem.confinement();
    assert_eq!(
        confinement.resolve(&path(&fixture, "alias/file")).unwrap(),
        fixture.root.join("real/file")
    );
    assert_eq!(
        confinement.resolve(&path(&fixture, "file-alias")).unwrap(),
        fixture.root.join("real/file")
    );
    assert_eq!(
        reason(
            &confinement
                .resolve(&path(&fixture, "escape/secret"))
                .unwrap_err()
        ),
        Some(PermissionReason::OutsideRoot)
    );
    for relative in [".state", ".STATE", "state-alias"] {
        assert_eq!(
            reason(&confinement.resolve(&path(&fixture, relative)).unwrap_err()),
            Some(PermissionReason::DaemonState)
        );
    }
}

#[test]
fn directory_resolution_pins_ancestors_and_rejects_reparse_replacement() {
    let fixture = fixture();
    fs::create_dir_all(fixture.root.join("parent/child")).unwrap();
    let pinned = fixture
        .filesystem
        .confinement()
        .pin_directory(&path(&fixture, "parent/child"))
        .unwrap();
    assert_eq!(pinned.path(), fixture.root.join("parent/child"));
    let error = fs::rename(fixture.root.join("parent"), fixture.outside.join("moved")).unwrap_err();
    assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
    drop(pinned);
    fs::rename(fixture.root.join("parent"), fixture.outside.join("moved")).unwrap();
    symlink_dir(&fixture.outside, fixture.root.join("parent")).unwrap();
    assert_eq!(
        reason(
            &fixture
                .filesystem
                .confinement()
                .resolve(&path(&fixture, "parent/secret"))
                .unwrap_err()
        ),
        Some(PermissionReason::OutsideRoot)
    );
}

#[test]
fn pinned_cwd_rejects_symlink_targets_that_dos_paths_would_normalize() {
    let fixture = fixture();
    // The extended native spelling creates a literal trailing-dot entry. Its
    // ordinary DOS spelling would instead resolve the adjacent outside link.
    let literal = PathBuf::from(format!(r"\\?\{}\literal.", fixture.root.display()));
    fs::create_dir(&literal).unwrap();
    symlink_dir(&fixture.outside, fixture.root.join("literal")).unwrap();
    symlink_dir("literal.", fixture.root.join("alias")).unwrap();
    let confinement = fixture.filesystem.confinement();
    assert_eq!(
        confinement.resolve(&path(&fixture, "alias")).unwrap(),
        fixture.root.join("literal.")
    );
    let error = confinement
        .pin_directory(&path(&fixture, "alias"))
        .unwrap_err();
    assert!(matches!(error.data, ErrorData::InvalidParams { .. }));
}

#[test]
fn recursive_removal_unlinks_reparse_objects_without_touching_targets() {
    let fixture = fixture();
    fs::create_dir_all(fixture.root.join("remove/nested")).unwrap();
    fs::write(fixture.root.join("remove/nested/file"), "delete").unwrap();
    fs::write(fixture.outside.join("keep"), "safe").unwrap();
    symlink_dir(&fixture.outside, fixture.root.join("remove/link")).unwrap();
    let directory = Directory::open_host(&fixture.root).unwrap();
    directory.remove_tree(OsStr::new("remove")).unwrap();
    assert!(!fixture.root.join("remove").exists());
    assert_eq!(fs::read(fixture.outside.join("keep")).unwrap(), b"safe");
}

#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_reads_writes_lists_searches_and_globs() {
    let fixture = fixture();
    let api = &fixture.filesystem;
    let context = RequestContext::detached();
    api.create_directory(
        &context,
        CreateDirectoryParams {
            path: path(&fixture, "src/deep"),
            recursive: true,
        },
    )
    .await
    .unwrap();
    let target = path(&fixture, "src/deep/雪.txt");
    api.write_file(
        &context,
        WriteFileParams {
            path: path(&fixture, "x"),
            data: b"short name".as_slice().into(),
            create: WriteCondition::IfAbsent,
            if_match: None,
            executable: false,
        },
    )
    .await
    .unwrap();
    let written = api
        .write_file(
            &context,
            WriteFileParams {
                path: target.clone(),
                data: b"target content\n".as_slice().into(),
                create: WriteCondition::IfAbsent,
                if_match: None,
                executable: false,
            },
        )
        .await
        .unwrap();
    let read = api
        .read_file(
            &context,
            ReadFileParams {
                path: target.clone(),
                offset: 0,
                max_bytes: 4096,
            },
        )
        .await
        .unwrap();
    assert_eq!(read.data.as_slice(), b"target content\n");
    assert_eq!(read.version, written.version);
    let listing = api
        .read_directory(
            &context,
            ReadDirectoryParams {
                path: path(&fixture, "src/deep"),
                cursor: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].name.as_str(), "雪.txt");
    let found = api
        .glob_files(
            &context,
            GlobFilesParams {
                root: path(&fixture, "."),
                filter: TreeFilter {
                    include: vec!["**/*.txt".into()],
                    ..Default::default()
                },
                limits: GlobFilesLimits {
                    max_matches: 100,
                    max_files: 100,
                    max_duration_ms: 10000,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    let searched = api
        .search_text(
            &context,
            SearchTextParams {
                root: path(&fixture, "."),
                pattern: "target".into(),
                pattern_ignore_case: false,
                filter: TreeFilter::default(),
                limits: SearchTextLimits {
                    max_matches: 100,
                    max_files: 100,
                    max_bytes: 4096,
                    max_duration_ms: 10000,
                    max_line_bytes: 1024,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(searched.matches.len(), 1);
    let error = api
        .write_file(
            &context,
            WriteFileParams {
                path: target,
                data: Vec::new().into(),
                create: WriteCondition::Always,
                if_match: None,
                executable: true,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
}

#[tokio::test(flavor = "current_thread")]
async fn native_rename_keeps_same_parent_atomic_and_refuses_cross_directory_moves() {
    let fixture = fixture();
    let context = RequestContext::detached();
    fs::create_dir(fixture.root.join("destination")).unwrap();
    fs::write(fixture.root.join("original"), b"source").unwrap();
    fs::write(fixture.root.join("destination/existing"), b"keep").unwrap();
    fixture
        .filesystem
        .rename(
            &context,
            RenameParams {
                source: path(&fixture, "original"),
                destination: path(&fixture, "renamed"),
                overwrite: false,
                if_match: None,
            },
        )
        .await
        .unwrap();
    assert!(!fixture.root.join("original").exists());
    assert_eq!(fs::read(fixture.root.join("renamed")).unwrap(), b"source");
    let error = fixture
        .filesystem
        .rename(
            &context,
            RenameParams {
                source: path(&fixture, "renamed"),
                destination: path(&fixture, "destination/existing"),
                overwrite: true,
                if_match: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
    assert_eq!(fs::read(fixture.root.join("renamed")).unwrap(), b"source");
    assert_eq!(
        fs::read(fixture.root.join("destination/existing")).unwrap(),
        b"keep"
    );
}

fn dacl(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, GetFileSecurityW};
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut required = 0;
    // SAFETY: the first query obtains the buffer size; the second fills that
    // allocation with a self-relative security descriptor without raw pointers.
    unsafe {
        GetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut required,
        )
    };
    assert!(required > 0);
    let mut descriptor = vec![0u8; required as usize];
    assert_ne!(
        unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        },
        0
    );
    descriptor
}

#[tokio::test(flavor = "current_thread")]
async fn native_atomic_write_preserves_existing_protected_acl() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    let fixture = fixture();
    let native = fixture.root.join("private");
    fs::write(&native, "old").unwrap();
    let handle = fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .open(&native)
        .unwrap();
    crate::windows_security::restrict_private_file(&handle).unwrap();
    drop(handle);
    let before = dacl(&native);
    fixture
        .filesystem
        .write_file(
            &RequestContext::detached(),
            WriteFileParams {
                path: path(&fixture, "private"),
                data: b"replacement".as_slice().into(),
                create: WriteCondition::IfPresent,
                if_match: None,
                executable: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(fs::read(&native).unwrap(), b"replacement");
    assert_eq!(dacl(&native), before);
}

#[tokio::test(flavor = "current_thread")]
async fn native_reads_stay_confined_during_directory_symlink_swaps() {
    use std::sync::{
        Barrier,
        atomic::{AtomicBool, Ordering},
    };
    let fixture = fixture();
    let racing = fixture.root.join("racing");
    let parked = fixture.root.join("parked");
    fs::create_dir(&racing).unwrap();
    fs::write(racing.join("file"), b"inside").unwrap();
    fs::write(fixture.outside.join("file"), b"outside secret").unwrap();
    let outside = fixture.outside.clone();
    let done = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(2));
    let thread_done = done.clone();
    let thread_barrier = barrier.clone();
    let attacker = std::thread::spawn(move || {
        thread_barrier.wait();
        let mut attempts = 0;
        while !thread_done.load(Ordering::Acquire) {
            attempts += 1;
            if fs::rename(&racing, &parked).is_ok() {
                symlink_dir(&outside, &racing).unwrap();
                std::thread::yield_now();
                loop {
                    match fs::remove_dir(&racing) {
                        Ok(()) => break,
                        // A resolver can briefly pin the reparse object before
                        // rejecting it. Retry once that handle is released.
                        Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => {
                            std::thread::yield_now()
                        }
                        Err(error) => panic!("cannot restore race fixture: {error}"),
                    }
                }
                fs::rename(&parked, &racing).unwrap();
            }
        }
        attempts
    });
    barrier.wait();
    let mut unexpected = Vec::new();
    for _ in 0..100 {
        let result = fixture
            .filesystem
            .read_file(
                &RequestContext::detached(),
                ReadFileParams {
                    path: path(&fixture, "racing/file"),
                    offset: 0,
                    max_bytes: 128,
                },
            )
            .await;
        match result {
            Ok(read) if read.data.as_slice() == b"inside" => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ProtocolErrorKind::PermissionDenied
                        | ProtocolErrorKind::NotFound
                        | ProtocolErrorKind::Unavailable
                ) || matches!(
                    error.data,
                    ErrorData::FailedPrecondition {
                        reason: PreconditionReason::SymlinkRejected
                    }
                ) => {}
            result => unexpected.push(format!("{result:?}")),
        }
    }
    done.store(true, Ordering::Release);
    assert!(attacker.join().unwrap() > 0);
    assert!(unexpected.is_empty(), "{unexpected:?}");
    assert_eq!(
        fs::read(fixture.outside.join("file")).unwrap(),
        b"outside secret"
    );
}
