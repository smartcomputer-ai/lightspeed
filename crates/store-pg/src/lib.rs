//! PostgreSQL-backed storage adapters.
//!
//! The first committed surface is the SQL schema. The Rust implementation will
//! layer the `agent-core` storage traits on top of this schema after review.

pub const INITIAL_SCHEMA_SQL: &str = include_str!("../migrations/001_initial.sql");
