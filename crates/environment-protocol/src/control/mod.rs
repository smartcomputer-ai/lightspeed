//! The controller plane: a provider controller creating, observing,
//! powering and closing targets for Lightspeed environments. The runtime
//! is the client; the provider is the server.
//!
//! The runtime connects with the provider credential as an
//! `Authorization: Bearer` header, on the controller URL and on every data
//! route the controller advertises; the provider checks it in constant time
//! before the WebSocket upgrade.
//!
//! **Bounded calls.** Every target method answers `{target: observation}`
//! promptly. Slow work (an image pull, a VM boot, a graceful shutdown)
//! continues in the background and is followed with `target/get`.
//!
//! **Ownership.** A target belongs to exactly one [`TargetOwner`](target::TargetOwner). Every
//! target method carries the owner; a provider resolves the target from
//! the owner when no `targetId` is given, and refuses a target that belongs
//! to another owner with `failedPrecondition` (`ownerMismatch`).

pub mod handshake;
pub mod ingress;
pub mod target;
pub mod template;
