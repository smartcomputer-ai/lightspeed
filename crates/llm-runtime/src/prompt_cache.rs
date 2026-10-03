//! Prompt-cache routing for providers that cache automatically.
//!
//! OpenAI caches request prefixes on its own, but routes requests across its
//! fleet by a caller-supplied `prompt_cache_key`; without one, hits on a
//! long-lived session are best-effort. The session id is the natural key:
//! every turn of a session shares its prefix, and nothing else does.

use harness::{BlobRef, SessionId};

/// OpenAI accepts short opaque keys; longer session ids are hashed so the
/// key stays stable and within bounds.
const MAX_PROMPT_CACHE_KEY_LEN: usize = 64;

pub(crate) fn prompt_cache_key(session_id: &SessionId) -> String {
    let id = session_id.as_str();
    if id.len() <= MAX_PROMPT_CACHE_KEY_LEN {
        return id.to_owned();
    }
    let digest = BlobRef::from_bytes(id.as_bytes());
    let hex = digest.as_str().rsplit(':').next().unwrap_or_default();
    format!("session-{}", &hex[..hex.len().min(56)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_session_ids_are_used_verbatim() {
        assert_eq!(
            prompt_cache_key(&SessionId::new("bot:v1:triage")),
            "bot:v1:triage"
        );
    }

    #[test]
    fn long_session_ids_hash_to_a_stable_short_key() {
        let id = SessionId::new("x".repeat(120));
        let key = prompt_cache_key(&id);
        assert!(key.len() <= MAX_PROMPT_CACHE_KEY_LEN);
        assert!(key.starts_with("session-"));
        assert_eq!(key, prompt_cache_key(&id));
    }
}
