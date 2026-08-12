//! Unix process-group lifecycle shared by job and process execution.
//!
//! Job and process commands start in their own process group so that
//! descendants a script leaves behind can be terminated as a unit when the
//! root process exits, is cancelled, or times out. A descendant can still
//! escape the group (`setsid`), so callers must never make completion depend
//! on the group being fully swept.

use std::time::Duration;

use tokio::process::Command;

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
    signal_group(pgid, 0)
}

/// Immediately kills every process left in the group.
pub(crate) fn kill_group(pgid: u32) -> bool {
    #[cfg(unix)]
    return signal_group(pgid, libc::SIGKILL);
    #[cfg(not(unix))]
    return signal_group(pgid, 0);
}

/// Terminates every process left in the group: SIGTERM, a bounded grace
/// period, then SIGKILL. Returns true when any process had to be signalled.
pub(crate) async fn sweep_group(pgid: u32) -> bool {
    #[cfg(unix)]
    {
        if !signal_group(pgid, libc::SIGTERM) {
            return false;
        }
        tokio::time::sleep(GROUP_TERM_GRACE).await;
        signal_group(pgid, libc::SIGKILL);
        true
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: i32) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return false;
    };
    unsafe { libc::kill(-pgid, signal) == 0 }
}

#[cfg(not(unix))]
fn signal_group(_pgid: u32, _signal: i32) -> bool {
    false
}
