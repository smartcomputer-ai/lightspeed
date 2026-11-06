//! Owned Windows Job Objects. Membership is supplied atomically at process
//! creation; numeric PIDs are diagnostic values, never authority to kill.

use std::{
    io, mem,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE},
    System::{JobObjects::*, Threading::*},
};

#[derive(Debug)]
pub(crate) struct Job(OwnedHandle);

impl Job {
    pub fn new() -> io::Result<Self> {
        // SAFETY: null security attributes produce a non-inheritable handle.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a newly owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Neither BREAKAWAY_OK nor SILENT_BREAKAWAY_OK is allowed.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits has the documented layout and lifetime.
        if unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    pub fn alive(&self) -> bool {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: info is a writable, correctly sized accounting structure.
        let ok = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                mem::size_of_val(&info) as u32,
                ptr::null_mut(),
            )
        };
        // A failed observation must never declare an environment idle.
        ok == 0 || info.ActiveProcesses != 0
    }

    pub fn terminate(&self) -> bool {
        // SAFETY: this owned handle names only this command's Job Object.
        unsafe { TerminateJobObject(self.raw(), 1) != 0 }
    }

    pub fn members(&self) -> Vec<(u32, Option<String>)> {
        let mut capacity = 32usize;
        loop {
            // The first two u32 values occupy one usize on our x64 target;
            // allocating usize words also provides the required alignment.
            let bytes = mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                + capacity * mem::size_of::<usize>();
            let mut storage = vec![0usize; bytes.div_ceil(mem::size_of::<usize>())];
            let info = storage
                .as_mut_ptr()
                .cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            // SAFETY: allocated storage is aligned and at least bytes long.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.raw(),
                    JobObjectBasicProcessIdList,
                    info.cast(),
                    bytes as u32,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                if io::Error::last_os_error().raw_os_error() == Some(234) && capacity < 1_048_576 {
                    capacity *= 2;
                    continue;
                }
                return Vec::new();
            }
            // SAFETY: the successful query initialized the header and this many
            // process identifiers, within the supplied allocation.
            let pids = unsafe {
                std::slice::from_raw_parts(
                    (*info).ProcessIdList.as_ptr(),
                    (*info).NumberOfProcessIdsInList as usize,
                )
            };
            let mut members: Vec<_> = pids
                .iter()
                .filter_map(|pid| u32::try_from(*pid).ok())
                .map(|pid| (pid, image_name(pid)))
                .collect();
            members.sort_by_key(|(pid, _)| *pid);
            return members;
        }
    }
}

fn process_handle(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: opens only query access; the result is owned, not inherited.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        None
    } else {
        Some(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    let process = process_handle(pid)?;
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: every output points to a FILETIME and the process handle is live.
    if unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn image_name(pid: u32) -> Option<String> {
    let process = process_handle(pid)?;
    let mut path = vec![0u16; 32768];
    let mut len = path.len() as u32;
    // SAFETY: path has len writable UTF-16 code units.
    if unsafe {
        QueryFullProcessImageNameW(process.as_raw_handle(), 0, path.as_mut_ptr(), &mut len)
    } == 0
    {
        return None;
    }
    String::from_utf16(&path[..len as usize]).ok()
}
