//! Protocol version numbers.
//!
//! Each number is matched exactly: a peer that states any other number is
//! refused with `versionMismatch`, and no range or negotiation exists. A
//! change to a plane's wire shape or to the meaning of an existing field or
//! method needs a new number for that plane. `release/metadata.env` carries
//! the same numbers and `scripts/release/verify-metadata.sh` checks them.

/// Version of the data plane and the registration channel.
///
/// Both share one number so a data-plane change also changes the number in
/// the gateway's `registration/challenge`, which is what triggers a
/// registered daemon's automatic upgrade.
pub const ENVD_PROTOCOL_VERSION: u32 = 4;

/// Version of the provider controller plane.
pub const CONTROLLER_PROTOCOL_VERSION: u32 = 4;
