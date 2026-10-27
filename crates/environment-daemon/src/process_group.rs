//! Unix process-group lifecycle shared by job and process execution.
//!
//! Every command starts as the leader of its own process group, so the
//! descendants a script leaves behind can be signalled as a unit. Jobs
//! sweep the group when the root exits; processes signal it only on
//! timeout or an explicit terminate or kill, and otherwise let leftovers
//! live with the environment. A descendant can still escape the group
//! (`setsid`), so completion never depends on the group being empty.

use std::time::Duration;

use tokio::process::Command;

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

/// How long remaining output is drained after the root process exits
/// before the execution settles without it.
#[cfg(not(test))]
pub(crate) const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
pub(crate) const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(200);

/// How often a terminating group is checked for members during its grace.
const GRACE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A signal the daemon sends to a whole process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupSignal {
    Interrupt,
    Terminate,
    Kill,
}

/// Places the spawned command in its own process group (the child becomes
/// the group leader).
pub(crate) fn spawn_in_own_group(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

/// True when at least one process is left in the group. Says nothing about
/// whose group it is; see [`Group::alive`].
pub(crate) fn group_alive(pgid: u32) -> bool {
    #[cfg(unix)]
    return group_pid(pgid).is_some_and(|pid| test_kill_process_group(pid).is_ok());
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// Sends `signal` to every process in the group. Returns true when the
/// group had at least one member to signal.
pub(crate) fn signal_group(pgid: u32, signal: GroupSignal) -> bool {
    #[cfg(unix)]
    {
        let signal = match signal {
            GroupSignal::Interrupt => Signal::INT,
            GroupSignal::Terminate => Signal::TERM,
            GroupSignal::Kill => Signal::KILL,
        };
        group_pid(pgid).is_some_and(|pid| kill_process_group(pid, signal).is_ok())
    }
    #[cfg(not(unix))]
    {
        let _ = (pgid, signal);
        false
    }
}

/// One spawned process group, named by its id and its leader's start time.
///
/// A group id stays reserved while the group has members, so a live group
/// whose leader is gone is still this group. Once the group empties, the id
/// may name a later, unrelated group whose leader got the same pid: a live
/// process with the group's pid and another start time means the group is
/// gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Group {
    pub pgid: u32,
    pub leader_start_time: Option<u64>,
}

impl Group {
    /// The group led by `pgid`, a live process (a just spawned root).
    pub fn capture(pgid: u32) -> Self {
        Self {
            pgid,
            leader_start_time: process_start_time(pgid),
        }
    }

    /// The group still has members, and they are this group's.
    pub fn alive(&self) -> bool {
        if !group_alive(self.pgid) {
            return false;
        }
        match process_start_time(self.pgid) {
            None => true,
            Some(started) => self.leader_start_time == Some(started),
        }
    }

    /// Sends `signal` to the group while it is alive. Returns true when it
    /// had a member to signal.
    pub fn signal(&self, signal: GroupSignal) -> bool {
        self.alive() && signal_group(self.pgid, signal)
    }

    /// Terminates every process left in the group: `SIGTERM`, up to
    /// `grace` for the members to exit, then `SIGKILL` to whatever remains.
    /// Returns true when the group had members to signal.
    pub async fn terminate(self, grace: Duration) -> bool {
        if !self.signal(GroupSignal::Terminate) {
            return false;
        }
        self.kill_after_grace(grace).await;
        true
    }

    /// Waits up to `grace` for the group to empty, then kills what is left.
    pub async fn kill_after_grace(self, grace: Duration) {
        let deadline = tokio::time::Instant::now() + grace;
        while self.alive() {
            if tokio::time::Instant::now() >= deadline {
                self.signal(GroupSignal::Kill);
                return;
            }
            tokio::time::sleep(GRACE_POLL_INTERVAL.min(grace)).await;
        }
    }
}

#[cfg(unix)]
fn group_pid(pgid: u32) -> Option<Pid> {
    // Group ids 0 and 1 have special kill semantics rather than naming a
    // child group.
    let pgid = i32::try_from(pgid).ok().filter(|pid| *pid > 1)?;
    Pid::from_raw(pgid)
}

/// Sends `SIGINT` to one process group by id, such as a terminal's
/// foreground group. Returns true when it had a member to signal.
pub(crate) fn interrupt_foreground(pgid: u32) -> bool {
    signal_group(pgid, GroupSignal::Interrupt)
}

/// This boot of the machine, so spawn records of an earlier boot are never
/// mistaken for live groups: Linux's boot id, macOS's boot time.
pub(crate) fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        let mut boot = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        // SAFETY: the name is NUL-terminated and the buffer is a timeval of
        // the size passed.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut boot as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (status == 0 && boot.tv_sec > 0)
            .then(|| format!("boottime:{}.{:06}", boot.tv_sec, boot.tv_usec))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// When process `pid` started, in a platform unit that only needs to be
/// stable for one boot: clock ticks since boot on Linux, microseconds since
/// the epoch on macOS. Tells a group leader apart from a later process that
/// reused its pid.
pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // "pid (comm) state ppid ..."; comm may contain spaces and
        // parentheses, so split after the last closing parenthesis. The
        // start time is field 22, the 20th after comm.
        let close = stat.rfind(')')?;
        stat[close + 1..]
            .split_whitespace()
            .nth(19)
            .and_then(|value| value.parse().ok())
    }
    #[cfg(target_os = "macos")]
    {
        let info = bsd_info(pid)?;
        Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Live, non-zombie members of a process group with their command lines,
/// lowest pid first. Best effort: `/proc` on Linux, libproc on macOS,
/// nothing elsewhere.
#[cfg(target_os = "linux")]
pub(crate) fn group_members(pgid: u32) -> Vec<(u32, Option<String>)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut members = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let mut fields = stat[close + 1..].split_whitespace();
        let state = fields.next();
        let _ppid = fields.next();
        let pgrp = fields.next().and_then(|value| value.parse::<u32>().ok());
        if pgrp != Some(pgid) || state == Some("Z") {
            continue;
        }
        let command = std::fs::read(entry.path().join("cmdline"))
            .ok()
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|command| !command.is_empty())
            .or_else(|| {
                let comm = stat.get(open + 1..close).unwrap_or_default();
                (!comm.is_empty()).then(|| comm.to_owned())
            });
        members.push((pid, command));
    }
    members.sort_by_key(|(pid, _)| *pid);
    members
}

#[cfg(target_os = "macos")]
pub(crate) fn group_members(pgid: u32) -> Vec<(u32, Option<String>)> {
    use std::{ffi::c_void, mem};

    let Ok(group) = libc::pid_t::try_from(pgid) else {
        return Vec::new();
    };
    let pid_size = mem::size_of::<libc::pid_t>();
    // libproc's group listing returns pid counts, not bytes: with a null
    // buffer the count of every process on the system, which is an upper
    // bound for the group, and with a buffer the count it filled.
    // SAFETY: a null buffer of size 0 only queries the count.
    let upper_bound = unsafe { libc::proc_listpgrppids(group, std::ptr::null_mut(), 0) };
    if upper_bound <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as libc::pid_t; upper_bound as usize + 16];
    // SAFETY: the buffer holds `pids.len()` pids and its byte size is passed.
    let count = unsafe {
        libc::proc_listpgrppids(
            group,
            pids.as_mut_ptr() as *mut c_void,
            (pids.len() * pid_size) as libc::c_int,
        )
    };
    if count <= 0 {
        return Vec::new();
    }
    pids.truncate(count as usize);
    let mut members = Vec::new();
    for pid in pids {
        let Ok(pid) = u32::try_from(pid) else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        if bsd_info(pid).is_some_and(|info| info.pbi_status == libc::SZOMB) {
            continue;
        }
        let mut path = vec![0u8; 4096];
        // SAFETY: the buffer is 4096 bytes and its length is passed.
        let len = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                path.as_mut_ptr() as *mut c_void,
                path.len() as u32,
            )
        };
        let command =
            (len > 0).then(|| String::from_utf8_lossy(&path[..len as usize]).into_owned());
        members.push((pid, command));
    }
    members.sort_by_key(|(pid, _)| *pid);
    members
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn group_members(_pgid: u32) -> Vec<(u32, Option<String>)> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    use std::{ffi::c_void, mem};

    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: proc_bsdinfo is plain data; zeroed is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is a proc_bsdinfo of the size passed.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut c_void,
            size,
        )
    };
    (read == size).then_some(info)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn process_groups_reject_special_and_out_of_range_ids() {
        for pgid in [0, 1, i32::MAX as u32 + 1, u32::MAX] {
            assert!(group_pid(pgid).is_none());
            assert!(!signal_group(pgid, GroupSignal::Kill));
        }
        assert_eq!(group_pid(42), Pid::from_raw(42));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_process_has_a_start_time_and_the_machine_a_boot_id() {
        assert!(process_start_time(std::process::id()).is_some());
        assert!(boot_id().is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminate_escalates_to_kill_when_the_group_ignores_sigterm() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "trap '' TERM; sleep 30"])
            .stdout(std::process::Stdio::null());
        spawn_in_own_group(&mut command);
        let mut child = command.spawn().expect("spawn");
        let pgid = child.id().expect("pid");
        // Give the shell time to install its trap.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        assert!(
            Group::capture(pgid)
                .terminate(Duration::from_millis(200))
                .await
        );
        let status = child.wait().await.expect("wait");
        assert!(started.elapsed() >= Duration::from_millis(150));
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(status.signal(), Some(Signal::KILL.as_raw()));
    }
}
