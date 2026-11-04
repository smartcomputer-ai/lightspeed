//! Shared limits for built-in tool execution.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolLimits {
    pub max_file_read_bytes: u64,
    pub max_model_visible_output_bytes: u64,
    pub max_process_output_bytes: u64,
    pub default_process_timeout_ms: u64,
    /// Deployment-owned ceiling on a requested `run_process` timeout. A caller
    /// may request a lower timeout but can never raise it above this bound;
    /// the hosted runtime derives its process activity deadline from the same
    /// ceiling (asserted by a temporal-runtime test).
    pub max_process_timeout_ms: u64,
    /// Deployment-owned bounds on recursive text search. The same
    /// bounds are sent to a native host search and enforced by the generic
    /// fallback; a caller may request fewer matches but can never raise any
    /// bound.
    pub max_search_matches: u64,
    pub max_search_files: u64,
    pub max_search_bytes: u64,
    pub max_search_duration_ms: u64,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_file_read_bytes: 512 * 1024 * 1024,
            max_model_visible_output_bytes: 64 * 1024,
            max_process_output_bytes: 512 * 1024,
            default_process_timeout_ms: 60_000,
            max_process_timeout_ms: 30 * 60 * 1000,
            max_search_matches: 1_000,
            max_search_files: 5_000,
            max_search_bytes: 64 * 1024 * 1024,
            max_search_duration_ms: 30_000,
        }
    }
}

/// Model-visible text after the projection budget, with the accounting
/// facts the completion event carries: how much the tool produced and
/// whether the budget cut it.
pub struct ProjectedText {
    pub bytes: Vec<u8>,
    pub output_bytes: u64,
    pub truncated: bool,
}

/// Cut model-visible text to `max_bytes` on a UTF-8 boundary, marking the cut.
pub fn projected(mut bytes: Vec<u8>, max_bytes: u64) -> ProjectedText {
    let output_bytes = bytes.len() as u64;
    let truncated = output_bytes > max_bytes;
    if truncated {
        bytes.truncate(max_bytes as usize);
        while std::str::from_utf8(&bytes).is_err() {
            bytes.pop();
        }
        bytes.extend_from_slice(b"\n[truncated]");
    }
    ProjectedText {
        bytes,
        output_bytes,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_cuts_on_a_character_boundary_and_keeps_the_true_size() {
        let whole = projected("héllo".as_bytes().to_vec(), 6);
        assert_eq!(whole.bytes, "héllo".as_bytes());
        assert_eq!(whole.output_bytes, 6);
        assert!(!whole.truncated);

        let cut = projected("héllo".as_bytes().to_vec(), 2);
        assert_eq!(cut.bytes, b"h\n[truncated]");
        assert_eq!(cut.output_bytes, 6);
        assert!(cut.truncated);
    }
}
