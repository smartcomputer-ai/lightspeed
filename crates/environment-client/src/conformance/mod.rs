//! Conformance suites that drive a real endpoint through this crate's
//! clients and check the protocol contract. Enabled by the `conformance`
//! feature.
//!
//! - [`controller`]: any provider controller, through
//!   [`ControllerClient`](crate::ControllerClient).

pub mod controller;
