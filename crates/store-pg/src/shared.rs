use harness::{
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

/// Wall-clock Unix milliseconds for catalog timestamps, clamped to bigint.
pub(crate) fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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

/// Postgres reports a value it cannot store with SQLSTATE class 22, such as 22P05
/// for a NUL in jsonb or 22021 for one in text. That is an answer about the data, so
/// it stays apart from the failures a retry can outlast.
pub(crate) fn session_sql_error(action: &str, error: sqlx::Error) -> SessionStoreError {
    let message = format!("{action}: {error}");
    let data_exception = error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code.starts_with("22"));
    if data_exception {
        SessionStoreError::DataRejected { message }
    } else {
        SessionStoreError::Store { message }
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::error::Error;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    #[derive(Debug)]
    struct Refusal(&'static str);

    impl fmt::Display for Refusal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "refused with {}", self.0)
        }
    }

    impl Error for Refusal {}

    impl DatabaseError for Refusal {
        fn message(&self) -> &str {
            "refused"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }

        fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn mapped(code: &'static str) -> SessionStoreError {
        session_sql_error(
            "insert session event",
            sqlx::Error::Database(Box::new(Refusal(code))),
        )
    }

    #[test]
    fn a_data_exception_is_a_rejection_and_keeps_its_words() {
        for code in ["22P05", "22021"] {
            let error = mapped(code);
            assert!(error.is_data_rejection(), "{code}: {error:?}");
            assert!(error.to_string().contains("insert session event"));
            assert!(error.to_string().contains(code));
        }
    }

    #[test]
    fn every_other_failure_stays_a_store_failure() {
        let unique_violation = mapped("23505");
        assert!(matches!(unique_violation, SessionStoreError::Store { .. }));
        let outage = session_sql_error("begin append transaction", sqlx::Error::PoolTimedOut);
        assert!(matches!(outage, SessionStoreError::Store { .. }));
        assert!(!outage.is_data_rejection());
    }
}
