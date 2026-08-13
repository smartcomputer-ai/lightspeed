//! Stable environment gateway route paths.

pub const ROUTE_PATH_PREFIX: &str = "/environment-gateway/routes";

/// Appended to a provider controller base URL for Lightspeed-initiated data
/// connections. The remainder is
/// `/{universe}/{binding}/{environment}/{incarnation}/{target}`.
pub const PROVIDER_DATA_PATH_PREFIX: &str = "/routes";
