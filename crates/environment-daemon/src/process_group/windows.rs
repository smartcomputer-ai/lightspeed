//! Owned Windows Job Objects. Membership is supplied atomically at process
//! creation; numeric PIDs are diagnostic values, never authority to kill.

use std::{
    io, mem,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
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

    pub async fn terminate_and_wait(&self) -> bool {
        // Retain process objects before termination: neither delayed teardown
        // nor PID reuse may make a completed job appear to have a live tree.
        let processes: Vec<_> = self
            .process_ids()
            .into_iter()
            .filter_map(|pid| {
                let process = process_handle(pid)?;
                self.contains(&process).then_some(process)
            })
            .collect();
        if !self.terminate() {
            return false;
        }
        while self.alive()
            || processes.iter().any(|process| {
                // SAFETY: retained handles have synchronization access.
                (unsafe { WaitForSingleObject(process.as_raw_handle(), 0) }) != WAIT_OBJECT_0
            })
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        true
    }

    fn contains(&self, process: &OwnedHandle) -> bool {
        let mut member = 0;
        // SAFETY: both owned handles and the writable boolean are live.
        let observed = unsafe { IsProcessInJob(process.as_raw_handle(), self.raw(), &mut member) };
        // Keep an uncertain observation; only a verified nonmember is excluded.
        observed == 0 || member != 0
    }

    pub fn members(&self) -> Vec<(u32, Option<String>)> {
        self.process_ids()
            .into_iter()
            .filter_map(|pid| {
                let Some(process) = process_handle(pid) else {
                    // An invalid PID has already exited; access denial leaves the
                    // member visible with no image name.
                    return (io::Error::last_os_error().raw_os_error()
                        != Some(ERROR_INVALID_PARAMETER as i32))
                    .then_some((pid, None));
                };
                // SAFETY: the process handle has synchronization access.
                if !self.contains(&process)
                    || unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0
                {
                    return None;
                }
                Some((pid, image_name(&process)))
            })
            .collect()
    }

    fn process_ids(&self) -> Vec<u32> {
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
                .collect();
            members.sort_unstable();
            return members;
        }
    }
}

fn process_handle(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: opens only query/wait access; the result is owned, not inherited.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
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
    // An exited process remains queryable while any observer retains a handle.
    // Its creation timestamp must not make it appear to be a live process.
    // SAFETY: the owned process handle has synchronization access; zero polls.
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_TIMEOUT {
        return None;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn image_name(process: &OwnedHandle) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn terminating_a_job_waits_for_its_retained_process_handles() {
        use std::os::windows::process::CommandExt;
        let spec = crate::exec::test_native_command("sleep");
        let mut child = std::process::Command::new(&spec.argv[0])
            .args(&spec.argv[1..])
            .envs(&spec.env)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let job = Job::new().unwrap();
        // SAFETY: both objects are owned; the helper only sleeps.
        assert_ne!(
            unsafe { AssignProcessToJobObject(job.raw(), child.as_raw_handle()) },
            0
        );
        assert_eq!(
            job.members()
                .iter()
                .map(|(pid, _)| *pid)
                .collect::<Vec<_>>(),
            [child.id()]
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(10), job.terminate_and_wait())
                .await
                .unwrap()
        );
        // Retaining Child prevents PID reuse and observes the actual object.
        assert_eq!(
            unsafe { WaitForSingleObject(child.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        assert!(!job.alive());
        assert!(job.members().is_empty());
        child.wait().unwrap();
    }

    #[test]
    fn retained_handle_does_not_make_an_exited_process_live() {
        let command = crate::exec::test_native_command("sleep");
        let mut child = std::process::Command::new(&command.argv[0])
            .args(&command.argv[1..])
            .envs(command.env)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let retained = process_handle(pid);
        let started = process_start_time(pid);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(retained.is_some());
        assert!(started.is_some());
        assert!(process_start_time(pid).is_none());
        drop(retained);
    }
}
