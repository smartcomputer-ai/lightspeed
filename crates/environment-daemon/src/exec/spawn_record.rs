//! Persisted spawn records: which process groups an earlier daemon
//! instance owned.
//!
//! A record names a group by its id, its leader's start time and the boot
//! it ran in. After a restart in the same boot, a group whose record still
//! matches is the earlier instance's: jobs kill it, processes adopt it as
//! leftovers. A group id cannot be reused while the group has members, so a
//! live group whose leader is gone is still the recorded one; a live leader
//! must also match its recorded start time.

use std::path::Path;

use environment_protocol::ids::DaemonInstanceId;
use serde::{Deserialize, Serialize};

use crate::process_group::{self, Group};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SpawnRecord {
    pub pgid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_start_time: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    pub daemon_instance_id: DaemonInstanceId,
}

impl SpawnRecord {
    /// A record of `group`, spawned by this daemon instance.
    pub fn new(group: Group, instance_id: &DaemonInstanceId) -> Self {
        Self {
            pgid: group.pgid,
            leader_start_time: group.leader_start_time,
            boot_id: process_group::boot_id(),
            daemon_instance_id: instance_id.clone(),
        }
    }

    /// The recorded group.
    pub fn group(&self) -> Group {
        Group {
            pgid: self.pgid,
            leader_start_time: self.leader_start_time,
        }
    }

    /// The record was written in `boot`, the current boot.
    pub fn same_boot(&self, boot: Option<&str>) -> bool {
        boot.is_some() && self.boot_id.as_deref() == boot
    }

    /// The recorded group still has members, and they are the recorded
    /// group's. Only meaningful for a record of the current boot.
    pub fn group_survives(&self) -> bool {
        self.group().alive()
    }
}

/// Writes `value` as JSON to `path` through a sibling temporary file, so a
/// crash leaves either the old or the new content.
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn a_live_group_survives_and_a_reused_leader_does_not() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        process_group::spawn_in_own_group(&mut command);
        let mut child = command.spawn().expect("spawn");
        let pgid = child.id().expect("pid");
        let instance = DaemonInstanceId::new("envd_test").unwrap();
        let record = SpawnRecord::new(Group::capture(pgid), &instance);
        assert!(record.same_boot(process_group::boot_id().as_deref()));
        assert!(!record.same_boot(None));
        assert!(record.group_survives());

        let reused = SpawnRecord {
            leader_start_time: record.leader_start_time.map(|started| started + 1),
            ..record.clone()
        };
        assert!(
            !reused.group_survives(),
            "another process with the same pid"
        );

        process_group::signal_group(pgid, process_group::GroupSignal::Kill);
        child.wait().await.expect("wait");
        assert!(!record.group_survives());
    }
}
