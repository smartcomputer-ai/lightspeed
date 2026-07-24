use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{BlobRef, EventSeq, PromiseId, PromiseResolution, RunId, RunStatus, SessionId};

const EMISSION_ID_PREFIX: &str = "emission:sha256:";
const EMISSION_ID_HEX_LEN: usize = 64;
const EMISSION_HASH_DOMAIN: &[u8] = b"lightspeed.emission.v1";

/// Stable identity for one cross-workflow emission.
///
/// Ids are deterministic digests over a domain-separated, length-prefixed
/// producer/source identity. They are safe to recompute after activity retry,
/// worker restart, or workflow continue-as-new.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmissionId(String);

impl EmissionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, EmissionIdError> {
        let value = value.into();
        if is_canonical_emission_id(&value) {
            Ok(Self(value))
        } else {
            Err(EmissionIdError::InvalidFormat { value })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_parts(kind: &[u8], parts: &[&[u8]]) -> Self {
        let mut digest = Sha256::new();
        update_digest_part(&mut digest, EMISSION_HASH_DOMAIN);
        update_digest_part(&mut digest, kind);
        for part in parts {
            update_digest_part(&mut digest, part);
        }
        Self(format!(
            "{EMISSION_ID_PREFIX}{}",
            hex::encode(digest.finalize())
        ))
    }

    pub fn for_run_terminal(
        universe_id: Uuid,
        session_id: &SessionId,
        run_id: RunId,
        token: &str,
    ) -> Self {
        let universe = universe_id.to_string();
        let run = run_id.as_u64().to_be_bytes();
        Self::from_parts(
            b"run_terminal",
            &[
                universe.as_bytes(),
                session_id.as_str().as_bytes(),
                &run,
                token.as_bytes(),
            ],
        )
    }

    pub fn for_source_resolution(
        universe_id: Uuid,
        workflow_id: &str,
        promise_id: &PromiseId,
    ) -> Self {
        let universe = universe_id.to_string();
        Self::from_parts(
            b"source_resolution",
            &[
                universe.as_bytes(),
                workflow_id.as_bytes(),
                promise_id.as_str().as_bytes(),
            ],
        )
    }
}

impl fmt::Display for EmissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for EmissionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EmissionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum EmissionIdError {
    #[error("emission id must use emission:sha256:<64 lowercase hex> format: {value}")]
    InvalidFormat { value: String },
}

/// Durable producer identity carried with every emission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EmissionProducer {
    /// A session-log-backed emission. `log_seq` is the exact sequence of the
    /// event that produced the fact.
    Session {
        universe_id: Uuid,
        session_id: SessionId,
        log_seq: EventSeq,
    },
    /// An emission backed by another durable workflow's state.
    /// `universe_id` scopes this communication edge; it does not imply that
    /// the producing workflow itself is universe-owned. A deployment-global
    /// workflow may emit independently authorized facts for multiple
    /// universes.
    Workflow {
        universe_id: Uuid,
        workflow_id: String,
    },
}

impl EmissionProducer {
    pub fn universe_id(&self) -> Uuid {
        match self {
            Self::Session { universe_id, .. } | Self::Workflow { universe_id, .. } => *universe_id,
        }
    }
}

/// Closed internal vocabulary carried by the shared delivery signal.
///
/// Workflow-port invocation bodies join this enum when the durable port
/// contracts land. This first slice folds the two existing transports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EmissionBody {
    RunTerminal {
        token: String,
        run_id: RunId,
        status: RunStatus,
        output_ref: Option<BlobRef>,
        failure_message_ref: Option<BlobRef>,
    },
    SourceResolution {
        promise_id: PromiseId,
        resolution: PromiseResolution,
    },
}

/// Bounded cross-workflow fact delivered through the fixed
/// `deliver_emission` signal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmissionEnvelope {
    pub emission_id: EmissionId,
    pub producer: EmissionProducer,
    pub body: EmissionBody,
}

impl EmissionEnvelope {
    #[allow(clippy::too_many_arguments)]
    pub fn run_terminal(
        universe_id: Uuid,
        session_id: SessionId,
        log_seq: EventSeq,
        token: String,
        run_id: RunId,
        status: RunStatus,
        output_ref: Option<BlobRef>,
        failure_message_ref: Option<BlobRef>,
    ) -> Self {
        let emission_id = EmissionId::for_run_terminal(universe_id, &session_id, run_id, &token);
        Self {
            emission_id,
            producer: EmissionProducer::Session {
                universe_id,
                session_id,
                log_seq,
            },
            body: EmissionBody::RunTerminal {
                token,
                run_id,
                status,
                output_ref,
                failure_message_ref,
            },
        }
    }

    pub fn source_resolution(
        universe_id: Uuid,
        workflow_id: String,
        promise_id: PromiseId,
        resolution: PromiseResolution,
    ) -> Self {
        let emission_id = EmissionId::for_source_resolution(universe_id, &workflow_id, &promise_id);
        Self {
            emission_id,
            producer: EmissionProducer::Workflow {
                universe_id,
                workflow_id,
            },
            body: EmissionBody::SourceResolution {
                promise_id,
                resolution,
            },
        }
    }
}

fn update_digest_part(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn is_canonical_emission_id(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(EMISSION_ID_PREFIX) else {
        return false;
    };
    hex.len() == EMISSION_ID_HEX_LEN
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn universe(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    #[test]
    fn run_terminal_ids_are_stable_and_universe_scoped() {
        let session_id = SessionId::new("session_1");
        let first =
            EmissionId::for_run_terminal(universe(1), &session_id, RunId::new(7), "promise_1");
        let retry =
            EmissionId::for_run_terminal(universe(1), &session_id, RunId::new(7), "promise_1");
        let other_universe =
            EmissionId::for_run_terminal(universe(2), &session_id, RunId::new(7), "promise_1");

        assert_eq!(first, retry);
        assert_ne!(first, other_universe);
        assert!(first.as_str().starts_with(EMISSION_ID_PREFIX));
    }

    #[test]
    fn source_resolution_ids_include_producer_and_promise() {
        let first = EmissionId::for_source_resolution(
            universe(1),
            "universe/envjob-a",
            &PromiseId::new("promise_1"),
        );
        let other_source = EmissionId::for_source_resolution(
            universe(1),
            "universe/envjob-b",
            &PromiseId::new("promise_1"),
        );
        let other_promise = EmissionId::for_source_resolution(
            universe(1),
            "universe/envjob-a",
            &PromiseId::new("promise_2"),
        );

        assert_ne!(first, other_source);
        assert_ne!(first, other_promise);
    }

    #[test]
    fn envelope_round_trips_and_rejects_noncanonical_ids() {
        let envelope = EmissionEnvelope::source_resolution(
            universe(1),
            "universe/envjob-a".to_owned(),
            PromiseId::new("promise_1"),
            PromiseResolution::Resolved {
                payload_ref: Some(BlobRef::from_bytes(b"done")),
            },
        );
        let encoded = serde_json::to_string(&envelope).expect("encode envelope");
        let decoded: EmissionEnvelope = serde_json::from_str(&encoded).expect("decode envelope");
        assert_eq!(decoded, envelope);

        let invalid = encoded.replace(envelope.emission_id.as_str(), "emission:sha256:ABC");
        assert!(serde_json::from_str::<EmissionEnvelope>(&invalid).is_err());
    }
}
