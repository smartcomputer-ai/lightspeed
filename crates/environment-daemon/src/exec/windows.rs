//! Native Windows spawning. Both pipes and ConPTY attach a non-breakaway Job
//! Object atomically with CreateProcessW; a child never runs outside ownership.

use super::{
    ExecOptions, Execution, OutputSink, PreparedCommand,
    input::{Input, InputSink},
};
use crate::process_group::{Group, windows::Job};
use environment_protocol::{
    data::exec::{ExecInput, OutputStream, PtySize},
    error::ProtocolError,
};
use portable_pty::MasterPty;
use std::{
    ffi::{OsStr, OsString, c_void},
    fs::File,
    io, mem,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, Mutex},
};
use windows_sys::Win32::{
    Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
    },
    Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal},
    Security::SECURITY_ATTRIBUTES,
    System::{Console::*, Pipes::CreatePipe, Threading::*},
};

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

/// Windows environment names use the OS's ordinal case comparison, including
/// Unicode names; ASCII folding is insufficient.
pub(super) fn same_env_name(left: &str, right: &str) -> bool {
    compare_names(OsStr::new(left), OsStr::new(right)) == std::cmp::Ordering::Equal
}

fn compare_names(left: &OsStr, right: &OsStr) -> std::cmp::Ordering {
    let left: Vec<_> = left.encode_wide().collect();
    let right: Vec<_> = right.encode_wide().collect();
    // SAFETY: both buffers have the explicitly supplied code-unit lengths.
    match unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    } {
        CSTR_EQUAL => std::cmp::Ordering::Equal,
        CSTR_LESS_THAN => std::cmp::Ordering::Less,
        _ => std::cmp::Ordering::Greater,
    }
}

fn environment(command: &PreparedCommand) -> Vec<(OsString, OsString)> {
    let mut values: Vec<_> = std::env::vars_os()
        .filter(|(name, _)| {
            !command
                .scrubbed_env
                .iter()
                .any(|removed| compare_names(name, OsStr::new(removed)).is_eq())
        })
        .collect();
    for (name, value) in command
        .env
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .chain(
            command
                .secret_env
                .iter()
                .map(|(n, v)| (n.as_str(), v.expose())),
        )
    {
        values.retain(|(key, _)| !compare_names(key, OsStr::new(name)).is_eq());
        values.push((name.into(), value.into()));
    }
    values.sort_by(|(left, _), (right, _)| compare_names(left, right));
    values
}

fn environment_block(values: &[(OsString, OsString)]) -> Vec<u16> {
    let mut result = Vec::new();
    for (name, value) in values {
        result.extend(name.encode_wide());
        result.push(b'=' as u16);
        result.extend(value.encode_wide());
        result.push(0);
    }
    if result.is_empty() {
        result.push(0);
    }
    result.push(0);
    result
}

fn executable(command: &PreparedCommand, env: &[(OsString, OsString)]) -> io::Result<PathBuf> {
    let program = Path::new(&command.argv[0]);
    let program = if program.extension().is_none() {
        program.with_extension("exe")
    } else {
        program.to_owned()
    };
    if !program
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "direct Windows execution requires an .exe; batch files need an explicit shell adapter",
        ));
    }
    if program.is_absolute() {
        return Ok(program);
    }
    if program.components().count() > 1 {
        return Ok(command.cwd.join(program));
    }
    let path = env
        .iter()
        .find(|(name, _)| compare_names(name, OsStr::new("PATH")).is_eq())
        .map(|(_, value)| value);
    if let Some(path) = path {
        for directory in std::env::split_paths(path) {
            let directory = if directory.is_absolute() {
                directory
            } else {
                command.cwd.join(directory)
            };
            let candidate = directory.join(&program);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "executable was not found in the command's PATH",
    ))
}

/// Standard Windows CRT argument grammar, preserving empty arguments, embedded
/// quotes and trailing backslashes. This never quotes a shell language.
fn command_line(argv: &[String]) -> Vec<u16> {
    let mut result = Vec::new();
    for (index, arg) in argv.iter().enumerate() {
        if index > 0 {
            result.push(b' ' as u16);
        }
        result.push(b'"' as u16);
        let mut slashes = 0;
        for unit in arg.encode_utf16() {
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            result.extend(std::iter::repeat_n(
                b'\\' as u16,
                if unit == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            result.push(unit);
            slashes = 0;
        }
        result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        result.push(b'"' as u16);
    }
    result.push(0);
    result
}

fn pipe() -> io::Result<(File, File)> {
    let mut read = ptr::null_mut();
    let mut write = ptr::null_mut();
    // SAFETY: two output handles; null attributes make them non-inheritable.
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe returned two new owned handles.
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}

struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: null output requests the allocation size.
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), count, 0, &mut size);
        }
        let mut value = Self {
            storage: vec![0; size.div_ceil(mem::size_of::<usize>())],
            initialized: false,
        };
        // SAFETY: aligned allocation has at least the required size.
        if unsafe { InitializeProcThreadAttributeList(value.raw(), count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        value.initialized = true;
        Ok(value)
    }
    fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    unsafe fn set(&mut self, attribute: u32, value: *const c_void, size: usize) -> io::Result<()> {
        // SAFETY: callers keep the correctly typed attribute value alive through spawn.
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw(),
                0,
                attribute as usize,
                value,
                size,
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: initialization succeeded and this is the sole owner.
            unsafe {
                DeleteProcThreadAttributeList(self.raw());
            }
        }
    }
}

struct ConsoleHandle(HPCON);
// SAFETY: the pseudoconsole handle can be moved across threads; access is serialized.
unsafe impl Send for ConsoleHandle {}
impl Drop for ConsoleHandle {
    fn drop(&mut self) {
        unsafe {
            ClosePseudoConsole(self.0);
        }
    }
}

pub(super) struct NativePty {
    console: Mutex<Option<ConsoleHandle>>,
    input: File,
    output: File,
    size: Mutex<portable_pty::PtySize>,
}

impl NativePty {
    fn open(size: PtySize) -> io::Result<Self> {
        let (input_read, input) = pipe()?;
        let (output, output_write) = pipe()?;
        let mut console = 0;
        // SAFETY: pipe handles remain valid during CreatePseudoConsole; flags 0
        // request the documented UTF-8 VT stream without private extension flags.
        let result = unsafe {
            CreatePseudoConsole(
                COORD {
                    X: size.cols as i16,
                    Y: size.rows as i16,
                },
                input_read.as_raw_handle(),
                output_write.as_raw_handle(),
                0,
                &mut console,
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "CreatePseudoConsole HRESULT {result:#x}"
            )));
        }
        Ok(Self {
            console: Mutex::new(Some(ConsoleHandle(console))),
            input,
            output,
            size: Mutex::new(portable_pty::PtySize {
                rows: size.rows as u16,
                cols: size.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            }),
        })
    }
    fn raw(&self) -> HPCON {
        self.console
            .lock()
            .unwrap()
            .as_ref()
            .expect("open console")
            .0
    }
    pub fn close(&self) {
        if let Some(console) = self.console.lock().unwrap().take() {
            // ClosePseudoConsole may wait for output to drain. Readers run on
            // other threads; never block a Tokio runtime thread on this close.
            let _ = std::thread::Builder::new()
                .name("envd-conpty-close".into())
                .spawn(move || drop(console));
        }
    }
}

impl MasterPty for NativePty {
    fn resize(&self, size: portable_pty::PtySize) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=4096).contains(&size.rows) && (1..=4096).contains(&size.cols),
            "invalid terminal dimensions"
        );
        let console = self.console.lock().unwrap();
        let console = console
            .as_ref()
            .ok_or_else(|| io::Error::other("terminal closed"))?;
        // SAFETY: the console is owned and the dimensions were checked.
        let result = unsafe {
            ResizePseudoConsole(
                console.0,
                COORD {
                    X: size.cols as i16,
                    Y: size.rows as i16,
                },
            )
        };
        anyhow::ensure!(result >= 0, "ResizePseudoConsole HRESULT {result:#x}");
        *self.size.lock().unwrap() = size;
        Ok(())
    }
    fn get_size(&self) -> anyhow::Result<portable_pty::PtySize> {
        Ok(*self.size.lock().unwrap())
    }
    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn io::Read + Send>> {
        Ok(Box::new(self.output.try_clone()?))
    }
    fn take_writer(&self) -> anyhow::Result<Box<dyn io::Write + Send>> {
        Ok(Box::new(self.input.try_clone()?))
    }
}

pub(super) struct Child {
    handle: OwnedHandle,
}
impl Child {
    pub fn wait(self) -> io::Result<std::process::ExitStatus> {
        // SAFETY: this handle remains owned throughout the wait and code query.
        if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(std::process::ExitStatus::from_raw(code))
    }
}

pub(super) fn spawn<S: OutputSink>(
    command: PreparedCommand,
    sink: S,
    options: ExecOptions,
    redactions: Vec<Vec<u8>>,
    capacity: u64,
) -> Result<Execution<S>, ProtocolError> {
    spawn_inner(command, sink, options, redactions, capacity)
        .map_err(|error| ProtocolError::process_failed(format!("native Windows spawn: {error}")))
}

fn spawn_inner<S: OutputSink>(
    command: PreparedCommand,
    sink: S,
    options: ExecOptions,
    redactions: Vec<Vec<u8>>,
    capacity: u64,
) -> io::Result<Execution<S>> {
    let environment = environment(&command);
    let executable = wide(executable(&command, &environment)?.as_os_str());
    let mut env = environment_block(&environment);
    let cwd = wide(command.cwd.as_os_str());
    let mut cmdline = command_line(&command.argv);
    let job = Arc::new(Job::new()?);
    let mut attributes = Attributes::new(2)?;
    let job_handle = job.raw();
    // SAFETY: job_handle remains alive until CreateProcessW completes.
    unsafe {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_JOB_LIST,
            (&job_handle as *const HANDLE).cast(),
            mem::size_of::<HANDLE>(),
        )?;
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    let mut child_handles = Vec::<File>::new();
    let mut inherit_handles = Vec::<HANDLE>::new();
    let mut readers = Vec::new();
    let input;
    let pty: Option<Box<dyn MasterPty + Send>>;
    if let Some(size) = command.pty {
        // Explicitly discard the daemon's redirected standard handles. Console
        // attachment replaces these invalid handles with the pseudoconsole's
        // handles; leaving them unspecified can retain the daemon's pipes.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        let terminal = NativePty::open(size)?;
        // SAFETY: pseudoconsole attribute takes the HPCON value itself.
        unsafe {
            attributes.set(
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                terminal.raw() as *const c_void,
                mem::size_of::<HPCON>(),
            )?;
        }
        readers.push((terminal.output.try_clone()?, OutputStream::Pty));
        let initial = match command.input {
            ExecInput::Bytes { data } => Some(data.into_inner()),
            _ => None,
        };
        input = Input::new(
            InputSink::Pty(Box::new(terminal.input.try_clone()?)),
            initial,
            false,
            true,
            capacity,
        );
        pty = Some(Box::new(terminal));
    } else {
        let (stdin_read, stdin_write) = pipe()?;
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin_read.as_raw_handle();
        startup.StartupInfo.hStdOutput = stdout_write.as_raw_handle();
        startup.StartupInfo.hStdError = stderr_write.as_raw_handle();
        child_handles.extend([stdin_read, stdout_write, stderr_write]);
        for handle in &child_handles {
            // SAFETY: only explicit stdio handles become inheritable. The spawn
            // attribute list limits inheritance to exactly these three handles.
            if unsafe {
                SetHandleInformation(
                    handle.as_raw_handle(),
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            inherit_handles.push(handle.as_raw_handle());
        }
        unsafe {
            attributes.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                inherit_handles.as_ptr().cast(),
                mem::size_of_val(inherit_handles.as_slice()),
            )?;
        }
        readers.extend([
            (stdout_read, OutputStream::Stdout),
            (stderr_read, OutputStream::Stderr),
        ]);
        input = match command.input {
            ExecInput::None {} => {
                drop(stdin_write);
                Input::none()
            }
            ExecInput::Bytes { data } => Input::new(
                InputSink::NativePipe(stdin_write),
                Some(data.into_inner()),
                true,
                false,
                0,
            ),
            ExecInput::Pipe {} => Input::new(
                InputSink::NativePipe(stdin_write),
                None,
                false,
                true,
                capacity,
            ),
        };
        pty = None;
    }
    startup.lpAttributeList = attributes.raw();
    let mut information = PROCESS_INFORMATION::default();
    let flags = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_UNICODE_ENVIRONMENT
        | if pty.is_none() { CREATE_NO_WINDOW } else { 0 };
    // SAFETY: all buffers and attribute values remain alive through this call.
    // The JOB_LIST attribute establishes ownership before the primary thread runs.
    if unsafe {
        CreateProcessW(
            executable.as_ptr(),
            cmdline.as_mut_ptr(),
            ptr::null::<SECURITY_ATTRIBUTES>(),
            ptr::null(),
            i32::from(pty.is_none()),
            flags,
            env.as_mut_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let child = Child {
        handle: unsafe { OwnedHandle::from_raw_handle(information.hProcess) },
    };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    drop(thread);
    drop(child_handles);
    let group = Group {
        pgid: information.dwProcessId,
        leader_start_time: crate::process_group::process_start_time(information.dwProcessId),
        job: Some(job),
    };
    Execution::start_native(group, child, pty, readers, input, sink, redactions, options)
}
