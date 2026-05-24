use engine::{
    BlobRef,
    session::{EventSeq, SessionPosition},
    storage::{BlobStoreError, SessionStoreError},
};

use crate::PgStoreError;

pub(crate) fn sha256_hex(blob_ref: &BlobRef) -> Result<&str, BlobStoreError> {
    let value = blob_ref.as_str();
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(BlobStoreError::Store {
            message: format!("unsupported blob ref format: {blob_ref}"),
        });
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(BlobStoreError::Store {
            message: format!("unsupported blob ref format: {blob_ref}"),
        });
    }
    Ok(digest)
}

pub(crate) fn u64_to_i64(value: u64, name: &str) -> Result<i64, SessionStoreError> {
    i64::try_from(value).map_err(|_| SessionStoreError::Store {
        message: format!("{name} is too large for Postgres bigint: {value}"),
    })
}

pub(crate) fn usize_to_session_i64(value: usize, name: &str) -> Result<i64, SessionStoreError> {
    i64::try_from(value).map_err(|_| SessionStoreError::Store {
        message: format!("{name} is too large for Postgres bigint: {value}"),
    })
}

pub(crate) fn usize_to_blob_i64(value: usize, name: &str) -> Result<i64, BlobStoreError> {
    i64::try_from(value).map_err(|_| BlobStoreError::Store {
        message: format!("{name} is too large for Postgres bigint: {value}"),
    })
}

pub(crate) fn i64_to_u64(value: i64, name: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{name} is negative in Postgres: {value}"))
}

pub(crate) fn event_seq_to_i64(seq: EventSeq) -> Result<i64, SessionStoreError> {
    u64_to_i64(seq.as_u64(), "event sequence")
}

pub(crate) fn optional_event_seq_to_i64(
    seq: Option<EventSeq>,
) -> Result<Option<i64>, BlobStoreError> {
    seq.map(|seq| {
        i64::try_from(seq.as_u64()).map_err(|_| BlobStoreError::Store {
            message: format!(
                "event sequence is too large for Postgres bigint: {}",
                seq.as_u64()
            ),
        })
    })
    .transpose()
}

pub(crate) fn session_position_from_i64(
    seq: Option<i64>,
) -> Result<Option<SessionPosition>, SessionStoreError> {
    seq.map(|seq| {
        i64_to_u64(seq, "head_seq").map(|seq| SessionPosition {
            seq: EventSeq::new(seq),
        })
    })
    .transpose()
    .map_err(|message| SessionStoreError::Store { message })
}

pub(crate) fn session_store_error(action: &str, error: PgStoreError) -> SessionStoreError {
    SessionStoreError::Store {
        message: format!("{action}: {error}"),
    }
}

pub(crate) fn session_sql_error(action: &str, error: sqlx::Error) -> SessionStoreError {
    SessionStoreError::Store {
        message: format!("{action}: {error}"),
    }
}

pub(crate) fn blob_store_error(action: &str, error: PgStoreError) -> BlobStoreError {
    BlobStoreError::Store {
        message: format!("{action}: {error}"),
    }
}

pub(crate) fn blob_sql_error(action: &str, error: sqlx::Error) -> BlobStoreError {
    BlobStoreError::Store {
        message: format!("{action}: {error}"),
    }
}

pub(crate) fn object_store_error(
    action: &str,
    key: &str,
    error: object_store::Error,
) -> BlobStoreError {
    BlobStoreError::Store {
        message: format!("{action} '{key}': {error}"),
    }
}
