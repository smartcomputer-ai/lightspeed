//! Fast in-process harnesses for tests and evals.
//!
//! This crate is not a production runtime and intentionally does not expose an
//! `api::AgentApiService` implementation.

pub mod runner;

pub use runner::*;
