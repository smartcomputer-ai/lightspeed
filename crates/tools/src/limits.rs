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
