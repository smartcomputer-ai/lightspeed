//! Protocol constants: the limits every hop enforces without reading a
//! handshake.
//!
//! Operational ceilings that a deployment configures (request concurrency,
//! retention, wait, scan and transfer maxima) are not constants: the data
//! plane advertises them in `initialize` as
//! [`DataLimits`](crate::data::handshake::DataLimits). Caller quotas are
//! required fields on each request, and a value above an advertised ceiling
//! is refused with `invalidParams` naming the field, never clamped.

/// Largest WebSocket message, and therefore frame, that any hop sends or
/// accepts on any plane. Every reader and writer configures it explicitly,
/// with the frame limit equal to the message limit. A sender that would
/// exceed it fails with `payloadTooLarge` instead of sending.
pub const MAX_MESSAGE_BYTES: u64 = 16 * 1024 * 1024;

/// A peer waiting for a response sends a WebSocket ping at least this often
/// while any request it sent is unanswered.
pub const KEEPALIVE_INTERVAL_MS: u64 = 20_000;

/// Intermediaries (gateways, relays, proxies) forward pings and pongs, count
/// them as activity, and never close a connection for idleness sooner than
/// this.
pub const MIN_INTERMEDIARY_IDLE_MS: u64 = 60_000;

/// Every data and controller server accepts at least this many requests in
/// flight on one connection before it answers `busy`.
pub const MIN_CONCURRENT_REQUESTS: u32 = 16;

/// A registration-channel peer that receives no frame for this many
/// `heartbeatIntervalMs` treats the connection as dead.
pub const HEARTBEAT_MISSED_INTERVALS: u32 = 3;

/// Longest [`EnvironmentPath`](crate::paths::EnvironmentPath) or
/// [`TreePath`](crate::paths::TreePath) in bytes.
pub const MAX_PATH_BYTES: u32 = 4096;

/// Longest [`DescriptiveText`](crate::metadata::DescriptiveText) in bytes:
/// a target's `message`, a template's `description`, a
/// `registration/closed` message.
pub const MAX_DESCRIPTIVE_TEXT_BYTES: u32 = 1024;

/// Longest URL a peer reports or is configured with (a data route base,
/// an ingress `endpoint`), in bytes.
pub const MAX_URL_BYTES: u32 = 2048;

/// Longest error `message` in bytes. [`ProtocolError::new`] cuts longer
/// messages on a character boundary; the message is never parsed.
///
/// [`ProtocolError::new`]: crate::error::ProtocolError::new
pub const MAX_ERROR_MESSAGE_BYTES: u32 = 4096;

/// Longest a provider data route holds a dial before it upgrades or
/// answers a [`DataRouteRefusal`](crate::routes::DataRouteRefusal), in
/// milliseconds: long enough to resume a paused target, short enough that
/// the caller's own wait stays in charge.
pub const MAX_DATA_ROUTE_HOLD_MS: u64 = 5_000;

/// Largest `Retry-After` a data route refusal names, in whole seconds.
/// Receivers clamp a larger value to it.
pub const RETRY_AFTER_MAX_SECS: u64 = 60;

/// Most chunks plus gaps in one `process/read` page, and across all entries
/// of one `job/read` response. A page that reaches it ends early and loses
/// nothing: `next` says where to continue.
pub const MAX_PAGE_CHUNKS: u32 = 4096;

/// Budget, in encoded bytes, for the variable part of a walk's result: the
/// matches of `fs/searchText` and `fs/globFiles`, and the entries, skips
/// and base64 content of all roots of one `fs/scan`. A walk stops (search
/// and glob: `stopped: responseLimit`) or a scan root fails
/// (`payloadTooLarge`) before its result would exceed it, so every response
/// stays below [`MAX_MESSAGE_BYTES`] with room for its fixed members.
pub const MAX_RESULT_BYTES: u64 = 12 * 1024 * 1024;

/// Most skipped entries a scan root or transfer lists by path; the total is
/// always counted in `skippedEntryCount`.
pub const MAX_LISTED_SKIPS: u32 = 100;

/// Most leftover processes a report lists; the rest are counted in
/// `omittedProcessCount`.
pub const MAX_LEFTOVER_PROCESSES: u32 = 64;

/// Longest leftover `command` in bytes; longer command lines are cut on a
/// character boundary.
pub const MAX_LEFTOVER_COMMAND_BYTES: u32 = 1024;

/// Longest field of an
/// [`ImplementationInfo`](crate::handshake::ImplementationInfo) in bytes.
pub const MAX_IMPLEMENTATION_FIELD_BYTES: u32 = 256;
