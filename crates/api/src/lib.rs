//! Client-facing API contracts for Lightspeed agents.
//!
//! This crate is intentionally independent of `engine` core types. Hosts
//! can implement these contracts from a local event-log runner, a Temporal
//! workflow gateway, or another substrate while clients keep speaking the same
//! session/run/item protocol.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

mod auth;
mod constants;
mod environments;
mod handshake;
mod ids;
mod mcp;
mod model;
mod notifications;
mod operator;
mod profiles;
mod rpc;
mod runs;
mod schema_export;
mod service;
mod sessions;
mod skills;
mod storage;
mod views;

pub use auth::*;
pub use constants::*;
pub use environments::*;
pub use handshake::*;
pub use ids::*;
pub use mcp::*;
pub use model::*;
pub use notifications::*;
pub use operator::*;
pub use profiles::*;
pub use rpc::*;
pub use runs::*;
pub use schema_export::{ExportedSchemas, export_schemas};
pub use service::*;
pub use sessions::*;
pub use skills::*;
pub use storage::*;
pub use views::*;

#[cfg(test)]
mod tests;
