//! Shared limits for built-in tool execution.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolLimits {
    pub max_file_read_bytes: u64,
    pub max_model_visible_output_bytes: u64,
    pub max_process_output_bytes: u64,
    pub default_process_timeout_ms: u64,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_file_read_bytes: 512 * 1024 * 1024,
            max_model_visible_output_bytes: 64 * 1024,
            max_process_output_bytes: 512 * 1024,
            default_process_timeout_ms: 60_000,
        }
    }
}
