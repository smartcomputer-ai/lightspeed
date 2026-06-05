//! CAS-backed virtual filesystem models and snapshot helpers.
//!
//! This crate owns deterministic VFS path and manifest structures plus helpers
//! that write immutable snapshot manifests into an injected `BlobStore`.
//! Host filesystem access, materialization, and process execution live outside
//! this crate.

pub mod catalog;
pub mod manifest;
pub mod path;
pub mod snapshot;

pub use catalog::*;
pub use manifest::*;
pub use path::*;
pub use snapshot::*;
