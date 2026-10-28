//! The controller plane: a provider controller creating, observing,
//! powering and closing targets for Lightspeed environments. The runtime
//! is the client; the provider is the server. Calls go from Lightspeed to
//! the provider only: the provider never calls Lightspeed.
//!
//! **Credential.** When the provider is registered with a bearer
//! credential, the runtime sends it as an `Authorization: Bearer` header on
//! the controller URL and on every data route; the provider checks it in
//! constant time before the WebSocket upgrade. Without a credential no
//! `Authorization` header is sent.
//!
//! **Bounded calls.** Every target method answers `{target: observation}`
//! promptly. Slow work (an image pull, a VM boot, a graceful shutdown)
//! continues in the background and is followed with `target/get`.
//!
//! **Ownership.** A target belongs to exactly one
//! [`TargetOwner`](target::TargetOwner), `{universeId, environmentId}`.
//! Every target method carries the owner; a provider resolves the target
//! from the owner when no `targetId` is given, and refuses a target that
//! belongs to another owner with `failedPrecondition` (`ownerMismatch`).
//!
//! **Provider obligations.** The provider owns its targets' power, idle
//! policy and ingress; Lightspeed decides only create and close. Every
//! provider:
//!
//! 1. makes `target/create` and `target/close` idempotent per owner;
//! 2. wakes a powered-down target when its data route is dialed, holding
//!    the upgrade at most
//!    [`MAX_DATA_ROUTE_HOLD_MS`](crate::limits::MAX_DATA_ROUTE_HOLD_MS)
//!    before it answers a [`DataRouteRefusal`](crate::routes::DataRouteRefusal);
//! 3. decides idleness from envd `env/getIdle`, so running processes, jobs
//!    and leftover process groups keep the target awake
//!    ([`idle`](crate::idle));
//! 4. never powers a target down while it relays a data connection to it;
//! 5. reports a target it removed on its own, such as by a lifetime limit
//!    or an idle `closeAfterMs`, as `closed`, or answers `notFound` from
//!    `target/get` once it no longer holds it;
//! 6. never snapshots or clones an envd state directory that holds a
//!    daemon identity.

pub mod handshake;
pub mod ingress;
pub mod target;
pub mod template;
