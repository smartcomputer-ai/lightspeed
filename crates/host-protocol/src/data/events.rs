//! Data-plane notifications.

use serde::{Deserialize, Serialize};

use crate::{
    data::process::{ProcessOutputChunk, ProcessOutputStream},
    shared::{ByteChunk, ProcessId},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessOutputNotification {
    pub process_id: ProcessId,
    pub seq: u64,
    pub stream: ProcessOutputStream,
    pub chunk: ByteChunk,
}

impl ProcessOutputNotification {
    pub fn from_chunk(process_id: ProcessId, chunk: ProcessOutputChunk) -> Self {
        Self {
            process_id,
            seq: chunk.seq,
            stream: chunk.stream,
            chunk: chunk.chunk,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExitedNotification {
    pub process_id: ProcessId,
    pub seq: u64,
    pub exit_code: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessClosedNotification {
    pub process_id: ProcessId,
    pub seq: u64,
}
