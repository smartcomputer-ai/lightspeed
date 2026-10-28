//! Wire types of the environment protocol.
//!
//! Three planes share one JSON-RPC 2.0 envelope ([`jsonrpc`]) and one error
//! vocabulary ([`error`]):
//!
//! - the **data plane** ([`data`]): the runtime drives envd's filesystem,
//!   processes and jobs; versioned by [`ENVD_PROTOCOL_VERSION`];
//! - the **controller plane** ([`control`]): the runtime drives a provider
//!   controller's targets; versioned by [`CONTROLLER_PROTOCOL_VERSION`];
//! - the **registration channel** ([`registration`]): a daemon registers
//!   with the gateway and is asked for reverse-dialed streams; versioned by
//!   [`ENVD_PROTOCOL_VERSION`].
//!
//! [`manifest`] lists every method with its types; [`idle`] holds the idle
//! policy providers run against envd's `env/getIdle`, and
//! [`routes::DataRouteRefusal`] the body a data route answers instead of
//! upgrading. The crate is transport-free: it defines messages and the
//! rules both sides follow, not sockets.
//!
//! # Conventions
//!
//! These rules hold for every plane; the frozen bootstrap surface
//! ([`registration::ChallengeParams`], [`bootstrap`]) is the one exception
//! to rule 13.
//!
//! 1. **Framing.** JSON-RPC 2.0 over WebSocket text frames; `initialize`
//!    first on the data and controller planes; requests may be pipelined
//!    and are matched by id.
//! 2. **Errors.** Integer codes fixed by kind, `data.kind` authoritative,
//!    typed fields instead of free-form details; [`error::ProtocolError`]
//!    displays as `{kind}: {message}`.
//! 3. **Methods.** `domain/verb[Object]` in lowerCamel with singular
//!    domains, batch-ness in params, no plane prefix; every method is a
//!    manifest entry; types are `<Verb><Object>Params` and
//!    `<Verb><Object>Response`. One exception: methods of a family that
//!    all act on one thing and answer its state share that state as their
//!    response type, and share one params type when they name only that
//!    thing (`TransferStatus` and `TransferOperationParams` for
//!    `transfer/*`, `TargetResponse` for `target/*` and `ingress/*`).
//! 4. **Fields.** camelCase; plural nouns for lists; `<qualifier><Noun>Count`
//!    for counts, `<qualifier>Bytes` for sizes, `…Ms` for durations,
//!    `…AtMs` for instants, `…Id`/`…Ids` for ids; payload bytes are always
//!    `data`; one name per relation (`dependsOn`, `truncated`). Positions
//!    name their unit: `nextOffset` (bytes of one file), `nextIndex`
//!    (entries), `nextCursor` (opaque); an output page's `next` and `end`
//!    are byte offsets into the handle's output stream.
//! 5. **Optionality.** No `null` except JSON-RPC's own `id`; absent options
//!    and empty collections are skipped; flags are `bool`, default false,
//!    skipped when false; no `Option<bool>`, no empty-string sentinels.
//! 6. **Numbers.** `u64` for bytes, offsets and durations; `u32` for counts,
//!    indices and depths; `i64` for instants; no `usize` on the wire; every
//!    value below 2^53.
//! 7. **Unions.** Data unions are tagged `kind`; variant fields are renamed
//!    only through enum-level `rename_all_fields`; operations are methods,
//!    never tagged actions.
//! 8. **Ids.** Validated newtypes ([`ids`]): the general grammar for
//!    Lightspeed-assigned ids, the opaque rule for provider-assigned ids.
//! 9. **Bytes.** Padded standard base64 for payloads; canonical lowercase
//!    hex for fixed-size crypto values; [`paths::ContentDigest`] as
//!    `sha256:<hex>`; opaque fingerprints carry their own prefix.
//! 10. **Paths.** [`paths::EnvironmentPath`] (absolute) for endpoint paths,
//!     [`paths::TreePath`] for relative tree entries.
//! 11. **Limits.** Caller quotas are required per request; endpoint ceilings
//!     are advertised in `initialize`; the message size is a protocol
//!     constant ([`limits`]); out-of-range values are rejected, never
//!     clamped. Ceilings are chosen so every response stays below the
//!     message limit: payload ceilings fit after base64, lists have length
//!     ceilings, walks stop at [`limits::MAX_RESULT_BYTES`], descriptive
//!     strings are bounded. [`jsonrpc::Response::to_text`]'s
//!     `payloadTooLarge` replacement is a defect backstop, not paging.
//! 12. **Capabilities.** In each plane's handshake only, as instance facts
//!     grouped by family; an absent family or fact is
//!     `capabilityUnavailable`; an endpoint never silently ignores a
//!     parameter.
//! 13. **Decoding.** Strict on every plane: unknown members are rejected,
//!     and defaults exist only for documented optional knobs.
//! 14. **Docs and fixtures.** Doc comments use wire names; every params and
//!     response type and every enum is documented; `fixtures/` holds one
//!     params, response and error fixture per method.
//! 15. **Boundaries.** Wire types are not persistence, workflow or
//!     model-facing types; each layer maps at its own boundary.

pub mod bootstrap;
pub mod bytes;
#[cfg(feature = "schema")]
pub mod contract;
pub mod control;
pub mod data;
pub mod error;
pub mod handshake;
pub mod idle;
pub mod ids;
pub mod jsonrpc;
pub mod limits;
pub mod manifest;
pub mod metadata;
pub mod paths;
pub mod registration;
pub mod routes;
mod serde_util;
mod version;

pub use version::{CONTROLLER_PROTOCOL_VERSION, ENVD_PROTOCOL_VERSION};
