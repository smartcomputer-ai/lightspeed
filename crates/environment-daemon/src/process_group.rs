//! Unix process-group lifecycle shared by job and process execution.
//!
//! Job and process commands start in their own process group so that
//! descendants a script leaves behind can be signalled as a unit. Jobs sweep
//! the group when the root exits; processes sweep it only on timeout, kill,
//! or cancellation, and otherwise let leftovers live with the environment. A
//! descendant can still escape the group (`setsid`), so callers must never
//! make completion depend on the group being fully swept.

use std::time::Duration;

use tokio::process::Command;

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

/// How long a terminated group gets between SIGTERM and SIGKILL.
#[cfg(not(test))]
pub(crate) const GROUP_TERM_GRACE: Duration = Duration::from_secs(1);
#[cfg(test)]
pub(crate) const GROUP_TERM_GRACE: Duration = Duration::from_millis(50);

/// How long remaining pipe output is drained after the root process exits
/// before the job or process completes without it.
#[cfg(not(test))]
pub(crate) const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
pub(crate) const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Places the spawned command in its own process group (the child becomes
/// the group leader).
pub(crate) fn spawn_in_own_group(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}

/// True when at least one process is left in the group.
pub(crate) fn group_alive(pgid: u32) -> bool {
    #[cfg(unix)]
    return group_pid(pgid).is_some_and(|pid| test_kill_process_group(pid).is_ok());
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// Immediately kills every process left in the group.
pub(crate) fn kill_group(pgid: u32) -> bool {
    #[cfg(unix)]
    return signal_group(pgid, Signal::KILL);
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// Terminates every process left in the group: SIGTERM, a bounded grace
/// period, then SIGKILL. Returns true when any process had to be signalled.
pub(crate) async fn sweep_group(pgid: u32) -> bool {
    #[cfg(unix)]
    {
        if !signal_group(pgid, Signal::TERM) {
            return false;
        }
        tokio::time::sleep(GROUP_TERM_GRACE).await;
        signal_group(pgid, Signal::KILL);
        true
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

#[cfg(unix)]
fn group_pid(pgid: u32) -> Option<Pid> {
    // Group IDs 0 and 1 have special kill semantics rather than naming a child group.
    let pgid = i32::try_from(pgid).ok().filter(|pid| *pid > 1)?;
    Pid::from_raw(pgid)
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: Signal) -> bool {
    group_pid(pgid).is_some_and(|pid| kill_process_group(pid, signal).is_ok())
}

/// Sends `SIGINT` to every process left in the group. Returns true when the
/// group had at least one member to signal.
pub(crate) fn interrupt_group(pgid: u32) -> bool {
    #[cfg(unix)]
    return signal_group(pgid, Signal::INT);
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn process_groups_reject_special_and_out_of_range_ids() {
        for pgid in [0, 1, i32::MAX as u32 + 1, u32::MAX] {
            assert!(group_pid(pgid).is_none());
        }
        assert_eq!(group_pid(42), Pid::from_raw(42));
    }
}
